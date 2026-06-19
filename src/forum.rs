//! Read-only client for the public Discourse JSON API at `forum.inderes.com`.
//!
//! Discourse serves public content as JSON whenever you append `.json` to a
//! page URL — no authentication required. This is deliberately **separate**
//! from the MCP/OAuth path: the Keycloak tokens minted for the `inderes-mcp`
//! client are not valid forum credentials, and Discourse runs its own session
//! model. We only ever read; we never send credentials here.
//!
//! Human output is best-effort (HTML stripped from post bodies). Agents doing
//! downstream work — e.g. sentiment analysis over posts — should use `--json`
//! to get the raw Discourse fields (`cooked`, `created_at`, `username`, …).

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Public Discourse instance. Overridable via `INDERES_FORUM_URL` (used by
/// tests to point at a mock server; also lets a self-hosted mirror be swapped
/// in without a recompile).
pub const DEFAULT_FORUM_URL: &str = "https://forum.inderes.com";

/// Resolve the forum base URL from the environment, falling back to the
/// public Inderes forum.
pub fn forum_base() -> String {
    std::env::var("INDERES_FORUM_URL").unwrap_or_else(|_| DEFAULT_FORUM_URL.to_string())
}

/// Minimal read-only HTTP client over the Discourse JSON API.
pub struct ForumClient<'a> {
    http: &'a reqwest::Client,
    base: String,
}

impl<'a> ForumClient<'a> {
    pub fn new(http: &'a reqwest::Client, base: &str) -> Self {
        Self {
            http,
            // Normalize so `format!("{base}{path}")` never doubles a slash.
            base: base.trim_end_matches('/').to_string(),
        }
    }

    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .get(&url)
            .query(query)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        // A 401/403 on an anonymous read is the signature of the forum being
        // switched to login-required (Discourse `login_required`). Diagnose it
        // explicitly rather than emitting a bare status — there is nothing the
        // user can do in this CLI to fix it (the forum's User-API-Key feature
        // is disabled), so point them at the actual lever.
        if matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            bail!(
                "forum.inderes.com denied anonymous access (HTTP {status}) — the forum may now \
                 require login. This CLI reads the forum anonymously; its User-API-Key feature \
                 is disabled, so authenticated access isn't currently possible. Ask Inderes to \
                 enable it."
            );
        }
        // Body is intentionally kept out of the error: Discourse error pages
        // can be large HTML, and we never want to splatter that at the user.
        if !status.is_success() {
            bail!("forum request to {path} returned HTTP {status}");
        }
        let body = resp.text().await.unwrap_or_default();
        serde_json::from_str(&body)
            .with_context(|| format!("parsing forum JSON response from {path}"))
    }

    /// Full-text search across topics and posts (`/search.json?q=`).
    pub async fn search(&self, query: &str) -> Result<Value> {
        self.get_json("/search.json", &[("q", query)]).await
    }

    /// A single topic with its post stream (`/t/<id>.json`).
    pub async fn topic(&self, id: &str) -> Result<Value> {
        let path = format!("/t/{id}.json");
        self.get_json(&path, &[]).await
    }

    /// Latest active topics (`/latest.json`).
    pub async fn latest(&self) -> Result<Value> {
        self.get_json("/latest.json", &[]).await
    }

    /// Forum categories (`/categories.json`).
    pub async fn categories(&self) -> Result<Value> {
        self.get_json("/categories.json", &[]).await
    }
}

// --- human-readable rendering ----------------------------------------------
//
// Each renderer returns a String so it can be unit-tested without touching
// stdout. The `--json` path bypasses all of these and prints the raw value.

/// Search results: one line per matched topic, with a stripped blurb from the
/// best-matching post when Discourse supplies one.
pub fn render_search(v: &Value) -> String {
    let topics = v.get("topics").and_then(Value::as_array);
    let posts = v.get("posts").and_then(Value::as_array);
    let Some(topics) = topics.filter(|t| !t.is_empty()) else {
        return "No matching topics.\n".to_string();
    };
    let mut out = String::new();
    for t in topics {
        let id = t.get("id").and_then(Value::as_i64).unwrap_or(0);
        let title = t
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)");
        let count = t.get("posts_count").and_then(Value::as_i64).unwrap_or(0);
        out.push_str(&format!("#{id}  {title}  ({count} posts)\n"));
        if let Some(blurb) = posts.and_then(|ps| blurb_for_topic(ps, id)) {
            out.push_str(&format!("    {}\n", truncate(&strip_html(&blurb), 200)));
        }
    }
    out.push_str("\nOpen a topic with: inderes forum topic <id>\n");
    out
}

/// A topic: title followed by every post in the stream, full text (HTML
/// stripped). Full bodies — not truncated — because the post text is the
/// payload for downstream analysis.
pub fn render_topic(v: &Value) -> String {
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("(untitled)");
    let mut out = format!("{title}\n");
    if let Some(id) = v.get("id").and_then(Value::as_i64) {
        out.push_str(&format!("topic #{id}\n"));
    }
    out.push('\n');

    let posts = v
        .get("post_stream")
        .and_then(|s| s.get("posts"))
        .and_then(Value::as_array);
    let Some(posts) = posts.filter(|p| !p.is_empty()) else {
        out.push_str("(no posts)\n");
        return out;
    };
    for p in posts {
        let n = p.get("post_number").and_then(Value::as_i64).unwrap_or(0);
        let who = p.get("username").and_then(Value::as_str).unwrap_or("?");
        let when = p.get("created_at").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("#{n} @{who} ({when}):\n"));
        let body = p.get("cooked").and_then(Value::as_str).unwrap_or("");
        out.push_str(&strip_html(body));
        out.push_str("\n\n");
    }
    out
}

/// Latest topics list (`topic_list.topics`).
pub fn render_latest(v: &Value) -> String {
    let topics = v
        .get("topic_list")
        .and_then(|l| l.get("topics"))
        .and_then(Value::as_array);
    let Some(topics) = topics.filter(|t| !t.is_empty()) else {
        return "No topics.\n".to_string();
    };
    let mut out = String::new();
    for t in topics {
        let id = t.get("id").and_then(Value::as_i64).unwrap_or(0);
        let title = t
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)");
        let count = t.get("posts_count").and_then(Value::as_i64).unwrap_or(0);
        let views = t.get("views").and_then(Value::as_i64).unwrap_or(0);
        out.push_str(&format!("#{id}  {title}  ({count} posts, {views} views)\n"));
    }
    out.push_str("\nOpen a topic with: inderes forum topic <id>\n");
    out
}

/// Categories list (`category_list.categories`).
pub fn render_categories(v: &Value) -> String {
    let cats = v
        .get("category_list")
        .and_then(|l| l.get("categories"))
        .and_then(Value::as_array);
    let Some(cats) = cats.filter(|c| !c.is_empty()) else {
        return "No categories.\n".to_string();
    };
    let mut out = String::new();
    for c in cats {
        let name = c.get("name").and_then(Value::as_str).unwrap_or("(unnamed)");
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("");
        let count = c.get("topic_count").and_then(Value::as_i64).unwrap_or(0);
        out.push_str(&format!("{name}  [{slug}]  ({count} topics)\n"));
        if let Some(desc) = c.get("description_text").and_then(Value::as_str) {
            if !desc.is_empty() {
                out.push_str(&format!("    {}\n", truncate(desc, 160)));
            }
        }
    }
    out
}

// --- pure helpers ----------------------------------------------------------

/// Find the `blurb` of the first search post belonging to `topic_id`.
fn blurb_for_topic(posts: &[Value], topic_id: i64) -> Option<String> {
    posts.iter().find_map(|p| {
        let same = p.get("topic_id").and_then(Value::as_i64) == Some(topic_id);
        if same {
            p.get("blurb")
                .and_then(Value::as_str)
                .filter(|b| !b.is_empty())
                .map(str::to_string)
        } else {
            None
        }
    })
}

/// Best-effort HTML → text: drop tags, collapse whitespace, decode the handful
/// of entities Discourse emits. Not a parser; good enough for terminal reading.
fn strip_html(s: &str) -> String {
    let mut text = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            _ => text.push(c),
        }
    }
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    decode_entities(&collapsed)
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&hellip;", "…")
        .replace("&nbsp;", " ")
}

/// Truncate to at most `max` chars (char-safe), appending `…` when cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // --- strip_html / decode_entities / truncate ---------------------------

    #[test]
    fn strip_html_removes_tags_and_collapses_whitespace() {
        let html = "<p>Hello   <b>world</b></p>\n<p>second</p>";
        assert_eq!(strip_html(html), "Hello world second");
    }

    #[test]
    fn strip_html_decodes_common_entities() {
        let html = "<p>Tom &amp; Jerry said &quot;hi&quot; &lt;here&gt;</p>";
        assert_eq!(strip_html(html), "Tom & Jerry said \"hi\" <here>");
    }

    #[test]
    fn strip_html_on_plain_text_is_identity_modulo_whitespace() {
        assert_eq!(strip_html("just text"), "just text");
    }

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn truncate_cuts_and_appends_ellipsis() {
        let out = truncate("abcdefghij", 5);
        assert_eq!(out, "abcde…");
    }

    #[test]
    fn truncate_is_char_safe_on_multibyte() {
        // Five multibyte chars, limit 3 — must not panic on a byte boundary.
        let out = truncate("äöåüö", 3);
        assert_eq!(out, "äöå…");
    }

    #[test]
    fn blurb_for_topic_matches_by_topic_id() {
        let posts = vec![
            json!({"topic_id": 1, "blurb": "first"}),
            json!({"topic_id": 2, "blurb": "second"}),
        ];
        assert_eq!(blurb_for_topic(&posts, 2).as_deref(), Some("second"));
        assert_eq!(blurb_for_topic(&posts, 99), None);
    }

    // --- renderers ---------------------------------------------------------

    #[test]
    fn render_search_lists_topics_with_blurbs() {
        let v = json!({
            "topics": [{"id": 42, "title": "Revenue beat", "posts_count": 7}],
            "posts": [{"topic_id": 42, "blurb": "<b>Strong</b> quarter"}]
        });
        let out = render_search(&v);
        assert!(out.contains("#42"));
        assert!(out.contains("Revenue beat"));
        assert!(out.contains("7 posts"));
        // Blurb HTML is stripped.
        assert!(out.contains("Strong quarter"));
        assert!(!out.contains("<b>"));
        assert!(out.contains("inderes forum topic"));
    }

    #[test]
    fn render_search_handles_no_results() {
        assert_eq!(
            render_search(&json!({"topics": []})),
            "No matching topics.\n"
        );
        assert_eq!(render_search(&json!({})), "No matching topics.\n");
    }

    #[test]
    fn render_topic_shows_full_post_bodies() {
        let v = json!({
            "id": 5,
            "title": "Outlook",
            "post_stream": {"posts": [
                {"post_number": 1, "username": "alice", "created_at": "2026-01-02",
                 "cooked": "<p>Bullish on margins</p>"},
                {"post_number": 2, "username": "bob", "created_at": "2026-01-03",
                 "cooked": "<p>Disagree</p>"}
            ]}
        });
        let out = render_topic(&v);
        assert!(out.contains("Outlook"));
        assert!(out.contains("topic #5"));
        assert!(out.contains("#1 @alice (2026-01-02):"));
        assert!(out.contains("Bullish on margins"));
        assert!(out.contains("#2 @bob"));
        assert!(out.contains("Disagree"));
        assert!(!out.contains("<p>"));
    }

    #[test]
    fn render_topic_handles_empty_stream() {
        let out = render_topic(&json!({"title": "Empty", "post_stream": {"posts": []}}));
        assert!(out.contains("Empty"));
        assert!(out.contains("(no posts)"));
    }

    #[test]
    fn render_latest_lists_topics() {
        let v = json!({"topic_list": {"topics": [
            {"id": 9, "title": "Daily thread", "posts_count": 120, "views": 4000}
        ]}});
        let out = render_latest(&v);
        assert!(out.contains("#9"));
        assert!(out.contains("Daily thread"));
        assert!(out.contains("120 posts"));
        assert!(out.contains("4000 views"));
    }

    #[test]
    fn render_categories_lists_names_and_descriptions() {
        let v = json!({"category_list": {"categories": [
            {"name": "Osakkeet", "slug": "osakkeet", "topic_count": 800,
             "description_text": "Keskustelua osakkeista"}
        ]}});
        let out = render_categories(&v);
        assert!(out.contains("Osakkeet"));
        assert!(out.contains("[osakkeet]"));
        assert!(out.contains("800 topics"));
        assert!(out.contains("Keskustelua osakkeista"));
    }

    // --- ForumClient (wiremock) -------------------------------------------

    #[tokio::test]
    async fn search_hits_search_endpoint_with_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search.json"))
            .and(query_param("q", "nokia"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "topics": [{"id": 1, "title": "Nokia", "posts_count": 3}],
                "posts": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let client = ForumClient::new(&http, &server.uri());
        let v = client.search("nokia").await.unwrap();
        assert_eq!(v["topics"][0]["title"], "Nokia");
    }

    #[tokio::test]
    async fn topic_hits_topic_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/t/123.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 123, "title": "Topic", "post_stream": {"posts": []}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let client = ForumClient::new(&http, &server.uri());
        let v = client.topic("123").await.unwrap();
        assert_eq!(v["id"], 123);
    }

    #[tokio::test]
    async fn non_success_status_is_an_error_without_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/t/404.json"))
            .respond_with(ResponseTemplate::new(404).set_body_string("<html>big error page</html>"))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let client = ForumClient::new(&http, &server.uri());
        let err = client.topic("404").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("404"), "got: {msg}");
        // The HTML body must not leak into the error.
        assert!(!msg.contains("big error page"), "got: {msg}");
    }

    #[tokio::test]
    async fn forbidden_is_diagnosed_as_login_required() {
        // The signature of the forum being flipped to login-required: an
        // anonymous read returns 403. We must explain it, not just echo 403.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/latest.json"))
            .respond_with(ResponseTemplate::new(403).set_body_string(r#"{"errors":["nope"]}"#))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let client = ForumClient::new(&http, &server.uri());
        let err = client.latest().await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("require login"), "got: {msg}");
        assert!(msg.contains("User-API-Key"), "got: {msg}");
    }

    #[test]
    fn new_trims_trailing_slash_from_base() {
        let http = reqwest::Client::new();
        let client = ForumClient::new(&http, "https://forum.example.com/");
        assert_eq!(client.base, "https://forum.example.com");
    }
}
