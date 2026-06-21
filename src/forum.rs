//! Forum helpers: mapping and rendering for the Inderes forum, read through the
//! hosted MCP server's `get-forum-posts` and `search-forum-topics` tools.
//!
//! Forum reads go over the same authenticated MCP channel as the rest of the
//! CLI — the server reads `forum.inderes.com` on the user's behalf, so an
//! `inderes login` (Premium) is required. This module is intentionally
//! transport-free: `commands.rs` owns the MCP calls and the SQLite cache, while
//! the pure functions here map a `get-forum-posts` post into the cache's row
//! shape, render a cached thread, and format `search-forum-topics` results.
//!
//! Post bodies arrive as markdown from the server (not Discourse HTML), so the
//! cache's clean-text `text` column needs little stripping; `strip_html` is kept
//! because the cache still runs it and a pre-MCP cache may hold HTML bodies.

use serde_json::{json, Value};

/// The public forum host. `get-forum-posts` requires a thread URL in the
/// `/t/<slug>/<id>` shape; the server routes by the trailing id and ignores the
/// slug, so [`thread_url`] uses a placeholder slug to address a topic by id.
pub const FORUM_HOST: &str = "https://forum.inderes.com";

/// Build a thread URL the `get-forum-posts` tool accepts from a bare topic id.
/// The slug is a placeholder (`-`): the server routes by the trailing id, so a
/// real slug is unnecessary and we don't have one when the user types an id.
pub fn thread_url(topic_id: i64) -> String {
    format!("{FORUM_HOST}/t/-/{topic_id}")
}

/// Extract the topic id from a forum thread URL. Search results carry a full
/// `/t/<slug>/<id>` URL but no bare id; the id is the last path segment that
/// parses as an integer (topic URLs have no trailing post number).
pub fn topic_id_from_url(url: &str) -> Option<i64> {
    url.trim_end_matches('/')
        .rsplit('/')
        .find_map(|seg| seg.parse::<i64>().ok())
}

/// Posts per `get-forum-posts` page. The tool caps `first`/`last` at 50.
pub const PAGE_SIZE: i64 = 50;

/// Build the `get-forum-posts` arguments to read one forward page of a thread.
/// `cursor` is the previous page's `endCursor`; `None` reads from the start.
/// Forward pagination (`first`/`after`) caches oldest→newest contiguously, so
/// the stored cursor is always a valid resume point.
pub fn page_request_args(thread_url: &str, cursor: Option<&str>) -> Value {
    let mut args = json!({ "threadUrl": thread_url, "first": PAGE_SIZE });
    if let Some(c) = cursor {
        args["after"] = json!(c);
    }
    args
}

/// One parsed page of a thread: the cache-ready posts plus the resume signal.
#[derive(Debug)]
pub struct PostsPage {
    pub posts: Vec<Value>,
    /// `pageInfo.endCursor` — where the next forward page resumes.
    pub next_cursor: Option<String>,
    /// `pageInfo.hasNextPage` — whether more posts follow.
    pub has_next: bool,
    /// `pageInfo.totalPosts` — the thread's current size, for topic metadata.
    pub total_posts: Option<i64>,
}

/// Pull the cache-ready posts and resume signal out of a `get-forum-posts`
/// `structuredContent` page. Posts are mapped to the cache row shape; the
/// `pageInfo` drives forward pagination.
pub fn parse_posts_page(sc: &Value) -> PostsPage {
    let posts = sc
        .get("posts")
        .and_then(Value::as_array)
        .map(|ps| ps.iter().map(mcp_post_to_cache).collect())
        .unwrap_or_default();
    let page_info = sc.get("pageInfo");
    PostsPage {
        posts,
        next_cursor: page_info
            .and_then(|pi| pi.get("endCursor"))
            .and_then(Value::as_str)
            .map(str::to_string),
        has_next: page_info
            .and_then(|pi| pi.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        total_posts: page_info
            .and_then(|pi| pi.get("totalPosts"))
            .and_then(Value::as_i64),
    }
}

/// Map one `get-forum-posts` post into the row shape the cache upserts. The
/// server's markdown `content` is stored in **both** `cooked` and `text`: it is
/// already clean, so it must NOT be HTML-stripped (stripping would treat
/// ordinary `<`/`>` in bodies like `P/E < 10` as tags and drop text). The
/// explicit `text` field signals "already clean" to the cache and renderer,
/// which otherwise derive `text` via `strip_html` for legacy HTML posts. `url`,
/// `score`, and `reply_count` are carried through so `--json`/SQL over `raw`
/// stay useful (e.g. ranking by `score`).
pub fn mcp_post_to_cache(p: &Value) -> Value {
    let content = p.get("content").and_then(Value::as_str);
    json!({
        "id": p.get("id").and_then(Value::as_i64),
        "post_number": p.get("postNumber").and_then(Value::as_i64),
        "username": p.get("username").and_then(Value::as_str),
        "created_at": p.get("createdAt").and_then(Value::as_str),
        "cooked": content,
        "text": content,
        "url": p.get("url").and_then(Value::as_str),
        "score": p.get("score"),
        "reply_count": p.get("replyCount"),
    })
}

// --- human-readable rendering ----------------------------------------------
//
// Each renderer returns a String so it can be unit-tested without touching
// stdout. The `--json` path bypasses all of these and prints the raw value.

/// A topic: title followed by every post in the stream, full body. Full bodies
/// — not truncated — because the post text is the payload for downstream
/// analysis. Bodies are markdown; `strip_html` is a no-op on markdown but
/// cleans up any HTML left from a pre-MCP cache.
pub fn render_topic(v: &Value) -> String {
    // No post_stream.posts at all = unexpected shape; dump raw JSON instead of
    // masquerading as an empty topic.
    let Some(posts) = v
        .get("post_stream")
        .and_then(|s| s.get("posts"))
        .and_then(Value::as_array)
    else {
        return fallback_json(v);
    };
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("(untitled)");
    let mut out = format!("{title}\n");
    if let Some(id) = v.get("id").and_then(Value::as_i64) {
        out.push_str(&format!("topic #{id}\n"));
    }
    out.push('\n');

    if posts.is_empty() {
        out.push_str("(no posts)\n");
        return out;
    }
    for p in posts {
        let n = p.get("post_number").and_then(Value::as_i64).unwrap_or(0);
        let who = p.get("username").and_then(Value::as_str).unwrap_or("?");
        let when = p.get("created_at").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("#{n} @{who} ({when}):\n"));
        // Prefer the pre-cleaned `text` (MCP markdown — must not be stripped);
        // fall back to stripping `cooked` for legacy HTML posts without it.
        if let Some(text) = p.get("text").and_then(Value::as_str) {
            out.push_str(text);
        } else {
            let body = p.get("cooked").and_then(Value::as_str).unwrap_or("");
            out.push_str(&strip_html(body));
        }
        out.push_str("\n\n");
    }
    out
}

/// `search-forum-topics` results: one line per matched thread, with the topic
/// id (parsed from its URL) so the result feeds straight into
/// `inderes forum topic <id>`.
pub fn render_forum_search(v: &Value) -> String {
    // Missing `topics` key = unexpected shape (dump raw); present-but-empty =
    // a genuine no-results.
    let Some(topics) = v.get("topics").and_then(Value::as_array) else {
        return fallback_json(v);
    };
    if topics.is_empty() {
        return "No matching topics.\n".to_string();
    }
    let mut out = String::new();
    for t in topics {
        let title = t
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)");
        let count = t.get("postsCount").and_then(Value::as_i64).unwrap_or(0);
        let id = t
            .get("url")
            .and_then(Value::as_str)
            .and_then(topic_id_from_url);
        match id {
            Some(id) => out.push_str(&format!("#{id}  {title}  ({count} posts)\n")),
            None => out.push_str(&format!("    {title}  ({count} posts)\n")),
        }
    }
    out.push_str("\nOpen a topic with: inderes forum topic <id>\n");
    out
}

/// Fallback for an unrecognized response shape: show the raw JSON so a silent
/// upstream change surfaces instead of masquerading as an empty result.
fn fallback_json(v: &Value) -> String {
    format!(
        "(unrecognized forum response shape — showing raw JSON; pass --json for the same)\n{}\n",
        serde_json::to_string_pretty(v).unwrap_or_default()
    )
}

// --- pure helpers ----------------------------------------------------------

/// Best-effort HTML → text for terminal reading: map block boundaries to
/// newlines (so multi-paragraph posts stay readable), drop remaining tags,
/// collapse intra-line whitespace while keeping line breaks, then decode the
/// handful of entities Discourse emits. Not a parser. Also used by the cache to
/// populate the clean-text `text` column. A no-op on plain markdown.
pub(crate) fn strip_html(s: &str) -> String {
    let with_breaks = s
        .replace("</p>", "\n\n")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</li>", "\n")
        .replace("</blockquote>", "\n");

    let mut text = String::with_capacity(with_breaks.len());
    let mut in_tag = false;
    for c in with_breaks.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            _ => text.push(c),
        }
    }

    // Collapse whitespace within each line but keep newlines; drop leading and
    // consecutive blank lines so the output isn't riddled with gaps.
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() && lines.last().is_none_or(|l: &String| l.is_empty()) {
            continue;
        }
        lines.push(collapsed);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    decode_entities(&lines.join("\n"))
}

fn decode_entities(s: &str) -> String {
    // `&amp;` must be decoded LAST: decoding it first would turn an encoded
    // reference like "&amp;gt;" into "&gt;" and then into ">", double-decoding it.
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&hellip;", "…")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- strip_html / decode_entities --------------------------------------

    #[test]
    fn strip_html_collapses_intra_line_whitespace_but_keeps_paragraphs() {
        // Tags gone, runs of spaces collapsed, but the paragraph break between
        // the two <p> blocks is preserved (one blank line).
        let html = "<p>Hello   <b>world</b></p>\n<p>second</p>";
        assert_eq!(strip_html(html), "Hello world\n\nsecond");
    }

    #[test]
    fn strip_html_converts_br_to_newline() {
        assert_eq!(strip_html("line one<br>line two"), "line one\nline two");
    }

    #[test]
    fn decode_entities_does_not_double_decode_amp() {
        // "&amp;gt;" encodes the literal text "&gt;" — it must decode to "&gt;",
        // not ">". Regression guard for &amp; being decoded before &gt;.
        assert_eq!(strip_html("&amp;gt;"), "&gt;");
    }

    #[test]
    fn strip_html_decodes_common_entities() {
        let html = "<p>Tom &amp; Jerry said &quot;hi&quot; &lt;here&gt;</p>";
        assert_eq!(strip_html(html), "Tom & Jerry said \"hi\" <here>");
    }

    #[test]
    fn strip_html_on_markdown_is_identity_modulo_whitespace() {
        // Server bodies are markdown: links/images pass through untouched.
        let md = "See [ip fray](https://ipfray.com) for **details**";
        assert_eq!(strip_html(md), md);
    }

    // --- thread_url / topic_id_from_url ------------------------------------

    #[test]
    fn thread_url_addresses_topic_by_id_with_placeholder_slug() {
        assert_eq!(thread_url(73687), "https://forum.inderes.com/t/-/73687");
    }

    #[test]
    fn topic_id_from_url_takes_the_trailing_numeric_segment() {
        assert_eq!(
            topic_id_from_url("https://forum.inderes.com/t/nokia-sijoituskohteena-osa-4/73687"),
            Some(73687)
        );
        // Trailing slash is tolerated.
        assert_eq!(
            topic_id_from_url("https://forum.inderes.com/t/x/74/"),
            Some(74)
        );
        // No numeric segment → None (renders without an id rather than wrong).
        assert_eq!(topic_id_from_url("https://forum.inderes.com/about"), None);
    }

    // --- mcp_post_to_cache -------------------------------------------------

    #[test]
    fn mcp_post_to_cache_maps_server_fields_to_cache_columns() {
        let mcp = json!({
            "id": 1159826,
            "url": "https://forum.inderes.com/t/x/73687/1226",
            "username": "Mustathmir",
            "createdAt": "2026-06-19T19:42:40.553Z",
            "content": "Nokia ja Acer ovat allekirjoittaneet sopimuksen.",
            "postNumber": 1226,
            "replyCount": 0,
            "score": 12.5
        });
        let row = mcp_post_to_cache(&mcp);
        assert_eq!(row["id"], 1159826);
        assert_eq!(row["post_number"], 1226);
        assert_eq!(row["username"], "Mustathmir");
        assert_eq!(row["created_at"], "2026-06-19T19:42:40.553Z");
        // Markdown body lands in both `cooked` and the pre-cleaned `text`
        // (so it is rendered/stored verbatim, never HTML-stripped).
        assert_eq!(
            row["cooked"],
            "Nokia ja Acer ovat allekirjoittaneet sopimuksen."
        );
        assert_eq!(
            row["text"],
            "Nokia ja Acer ovat allekirjoittaneet sopimuksen."
        );
        // Extras carried through for --json / SQL over raw.
        assert_eq!(row["url"], "https://forum.inderes.com/t/x/73687/1226");
        assert_eq!(row["score"], 12.5);
        assert_eq!(row["reply_count"], 0);
    }

    // --- pagination helpers ------------------------------------------------

    #[test]
    fn page_request_args_omits_after_on_the_first_page() {
        let a = page_request_args("https://forum.inderes.com/t/-/74", None);
        assert_eq!(a["threadUrl"], "https://forum.inderes.com/t/-/74");
        assert_eq!(a["first"], 50);
        assert!(
            a.get("after").is_none(),
            "first page must not send a cursor"
        );
    }

    #[test]
    fn page_request_args_resumes_from_cursor() {
        let a = page_request_args("https://forum.inderes.com/t/-/74", Some("c42"));
        assert_eq!(a["after"], "c42");
        assert_eq!(a["first"], 50);
    }

    #[test]
    fn parse_posts_page_maps_posts_and_reads_pageinfo() {
        let sc = json!({
            "posts": [
                {"id": 1, "postNumber": 1, "username": "a", "createdAt": "2026-01-01",
                 "content": "first", "url": "u1", "replyCount": 0, "score": 0},
                {"id": 2, "postNumber": 2, "username": "b", "createdAt": "2026-01-02",
                 "content": "second", "url": "u2", "replyCount": 1, "score": 3}
            ],
            "pageInfo": {"endCursor": "2", "hasNextPage": true, "totalPosts": 1176}
        });
        let page = parse_posts_page(&sc);
        assert_eq!(page.posts.len(), 2);
        assert_eq!(page.posts[0]["post_number"], 1);
        assert_eq!(page.posts[0]["cooked"], "first");
        assert_eq!(page.next_cursor.as_deref(), Some("2"));
        assert!(page.has_next);
        assert_eq!(page.total_posts, Some(1176));
    }

    #[test]
    fn parse_posts_page_handles_a_terminal_page() {
        // Last page: no more posts to follow.
        let sc = json!({"posts": [], "pageInfo": {"endCursor": "9", "hasNextPage": false}});
        let page = parse_posts_page(&sc);
        assert!(page.posts.is_empty());
        assert!(!page.has_next);
        assert_eq!(page.next_cursor.as_deref(), Some("9"));
        assert_eq!(page.total_posts, None);
    }

    // --- renderers ---------------------------------------------------------

    #[test]
    fn render_forum_search_lists_topics_with_ids_from_url() {
        let v = json!({"topics": [
            {"title": "Nokia sijoituskohteena (Osa 4)", "postsCount": 1176,
             "url": "https://forum.inderes.com/t/nokia-sijoituskohteena-osa-4/73687"}
        ]});
        let out = render_forum_search(&v);
        assert!(out.contains("#73687"), "got: {out}");
        assert!(out.contains("Nokia sijoituskohteena (Osa 4)"));
        assert!(out.contains("1176 posts"));
        assert!(out.contains("inderes forum topic"));
    }

    #[test]
    fn render_forum_search_empty_topics_is_a_genuine_no_result() {
        assert_eq!(
            render_forum_search(&json!({"topics": []})),
            "No matching topics.\n"
        );
    }

    #[test]
    fn render_unrecognized_shape_dumps_raw_json_not_a_fake_empty() {
        // `topics`/`post_stream` missing entirely = unexpected shape; surface it
        // instead of pretending it's empty.
        for out in [
            render_forum_search(&json!({"unexpected": 1})),
            render_topic(&json!({"unexpected": 1})),
        ] {
            assert!(out.contains("unrecognized"), "got: {out}");
            assert!(out.contains("unexpected"), "got: {out}");
        }
    }

    #[test]
    fn render_topic_shows_full_post_bodies() {
        let v = json!({
            "id": 5,
            "title": "Outlook",
            "post_stream": {"posts": [
                {"post_number": 1, "username": "alice", "created_at": "2026-01-02",
                 "cooked": "Bullish on margins"},
                {"post_number": 2, "username": "bob", "created_at": "2026-01-03",
                 "cooked": "Disagree"}
            ]}
        });
        let out = render_topic(&v);
        assert!(out.contains("Outlook"));
        assert!(out.contains("topic #5"));
        assert!(out.contains("#1 @alice (2026-01-02):"));
        assert!(out.contains("Bullish on margins"));
        assert!(out.contains("#2 @bob"));
        assert!(out.contains("Disagree"));
    }

    #[test]
    fn render_topic_does_not_html_strip_markdown_bodies() {
        // Regression: a markdown body carries a pre-cleaned `text`. It must be
        // rendered verbatim — `strip_html` would treat `< 10` / `> B` as tags
        // and silently drop the surrounding text.
        let v = json!({
            "id": 5,
            "title": "Valuation",
            "post_stream": {"posts": [
                {"post_number": 1, "username": "alice", "created_at": "2026-01-02",
                 "cooked": "P/E < 10 and EV/EBITDA > 5",
                 "text": "P/E < 10 and EV/EBITDA > 5"}
            ]}
        });
        let out = render_topic(&v);
        assert!(out.contains("P/E < 10 and EV/EBITDA > 5"), "got: {out}");
    }

    #[test]
    fn render_topic_handles_empty_stream() {
        let out = render_topic(&json!({"title": "Empty", "post_stream": {"posts": []}}));
        assert!(out.contains("Empty"));
        assert!(out.contains("(no posts)"));
    }
}
