//! CLI subcommand implementations.
//!
//! Each subcommand maps its ergonomic arguments to one MCP tool call on
//! `mcp.inderes.com`. See `docs/tools` on the server for the full list of
//! tools; for the ones not covered by a friendly subcommand, use
//! `inderes call <tool> --arg k=v`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap_complete::Shell;
use serde_json::{json, Map, Value};

use crate::auth;
use crate::cache;
use crate::forum;
use crate::mcp::McpClient;
use crate::oauth::{self, IdpConfig};
use crate::skill;
use crate::storage;

// --- login / logout / whoami ------------------------------------------------

pub async fn login(
    http: &reqwest::Client,
    idp: &IdpConfig,
    no_browser: bool,
    paste_callback: bool,
) -> Result<()> {
    let mode = if paste_callback {
        oauth::LoginMode::PasteCallback
    } else {
        oauth::LoginMode::Loopback
    };
    let tokens = oauth::login(http, idp, oauth::DEFAULT_SCOPES, !no_browser, mode).await?;
    storage::save(&tokens)?;
    let ui = oauth::userinfo(http, idp, &tokens.access_token).await.ok();
    let who = ui
        .as_ref()
        .and_then(|v| v.get("preferred_username").and_then(|s| s.as_str()))
        .or_else(|| {
            ui.as_ref()
                .and_then(|v| v.get("email").and_then(|s| s.as_str()))
        })
        .unwrap_or("unknown");
    println!("Signed in as {who}.");
    Ok(())
}

pub fn logout() -> Result<()> {
    storage::clear()?;
    println!("Signed out.");
    Ok(())
}

pub async fn whoami(http: &reqwest::Client, idp: &IdpConfig, verbose: bool) -> Result<()> {
    let Some(tokens) = auth::load_stored()? else {
        println!("Not signed in. Run `inderes login`.");
        return Ok(());
    };
    let remaining = (tokens.expires_at - time::OffsetDateTime::now_utc()).whole_seconds();
    if remaining > 0 {
        println!("Signed in. Access token valid for {remaining}s.");
    } else {
        println!("Signed in (refresh required on next call).");
    }
    if verbose {
        println!("Storage: {}", storage::backend_description());
        match oauth::userinfo(http, idp, &tokens.access_token).await {
            Ok(ui) => println!("{}", serde_json::to_string_pretty(&ui)?),
            Err(e) => eprintln!("userinfo call failed: {e:#}"),
        }
    }
    Ok(())
}

// --- friendly subcommands --------------------------------------------------

pub struct ToolCtx<'a> {
    pub http: &'a reqwest::Client,
    pub endpoint: &'a str,
    pub idp: &'a IdpConfig,
    pub json_output: bool,
}

impl<'a> ToolCtx<'a> {
    async fn client(&self) -> Result<McpClient> {
        let token = auth::ensure_access_token(self.http, self.idp).await?;
        let mut c = McpClient::new(self.http.clone(), self.endpoint, token);
        c.initialize().await.context("initializing MCP session")?;
        Ok(c)
    }

    async fn call(&self, tool: &str, args: Value) -> Result<()> {
        let mut c = self.client().await?;
        let result = c.call_tool(tool, args).await;
        // Best-effort session cleanup — don't let a failed DELETE turn a
        // successful tool call into an error.
        let _ = c.close().await;
        print_result(&result?, self.json_output)
    }

    /// Call a tool and return its raw result envelope, erroring on an
    /// `isError` result. For callers that need the structured data (to cache or
    /// re-render) rather than printing it straight through like [`call`].
    async fn call_raw(&self, tool: &str, args: Value) -> Result<Value> {
        let mut c = self.client().await?;
        let result = c.call_tool(tool, args).await;
        let _ = c.close().await;
        let result = result?;
        if let Some(msg) = result_error_text(&result) {
            bail!("MCP tool {tool} returned an error: {msg}");
        }
        Ok(result)
    }
}

/// If an MCP result is an error (`isError: true`), return its rendered text for
/// a diagnostic; otherwise `None`. Keeps the error message off the happy path.
fn result_error_text(result: &Value) -> Option<String> {
    if result.get("isError").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let mut buf = Vec::new();
    let _ = render_result(result, &mut buf);
    Some(String::from_utf8_lossy(&buf).trim().to_string())
}

/// Extract a tool result's structured payload: `structuredContent` if present,
/// else the first text content item parsed as JSON. Forum fetching and search
/// both need the typed object, not the human-rendered text.
fn structured_content(result: &Value) -> Result<Value> {
    if let Some(sc) = result.get("structuredContent") {
        return Ok(sc.clone());
    }
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|i| {
                (i.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| i.get("text").and_then(Value::as_str))
                    .flatten()
            })
        })
        .ok_or_else(|| anyhow!("MCP result had no structuredContent or text content"))?;
    serde_json::from_str(text).context("parsing MCP text content as JSON")
}

pub async fn search(ctx: &ToolCtx<'_>, query: &str) -> Result<()> {
    ctx.call("search-companies", json!({ "query": query }))
        .await
}

/// Build the `get-fundamentals` arguments. Pure so the arg shape is testable
/// without a live MCP call; the async wrapper just forwards it.
fn fundamentals_args(
    company_ids: Vec<String>,
    resolution: &str,
    fields: Vec<String>,
    start_year: Option<i32>,
    end_year: Option<i32>,
) -> Value {
    let mut args = Map::new();
    args.insert(
        "companyIds".into(),
        Value::Array(company_ids.into_iter().map(Value::String).collect()),
    );
    args.insert("resolution".into(), Value::String(resolution.into()));
    if !fields.is_empty() {
        args.insert(
            "fields".into(),
            Value::Array(fields.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(y) = start_year {
        args.insert("startYear".into(), y.into());
    }
    if let Some(y) = end_year {
        args.insert("endYear".into(), y.into());
    }
    Value::Object(args)
}

pub async fn fundamentals(
    ctx: &ToolCtx<'_>,
    company_ids: Vec<String>,
    resolution: &str,
    fields: Vec<String>,
    start_year: Option<i32>,
    end_year: Option<i32>,
) -> Result<()> {
    let args = fundamentals_args(company_ids, resolution, fields, start_year, end_year);
    ctx.call("get-fundamentals", args).await
}

/// Build the `get-inderes-estimates` arguments.
fn estimates_args(
    company_ids: Vec<String>,
    fields: Vec<String>,
    count: u32,
    quarters: bool,
    year_count: u32,
) -> Value {
    let mut args = Map::new();
    if !company_ids.is_empty() {
        args.insert(
            "companyIds".into(),
            Value::Array(company_ids.into_iter().map(Value::String).collect()),
        );
    }
    args.insert(
        "fields".into(),
        Value::Array(fields.into_iter().map(Value::String).collect()),
    );
    args.insert("count".into(), count.into());
    args.insert("includeQuarters".into(), quarters.into());
    args.insert("yearCount".into(), year_count.into());
    Value::Object(args)
}

pub async fn estimates(
    ctx: &ToolCtx<'_>,
    company_ids: Vec<String>,
    fields: Vec<String>,
    count: u32,
    quarters: bool,
    year_count: u32,
) -> Result<()> {
    let args = estimates_args(company_ids, fields, count, quarters, year_count);
    ctx.call("get-inderes-estimates", args).await
}

/// Build the `list-content` arguments.
fn content_list_args(
    company_id: Option<String>,
    types: Vec<String>,
    first: u32,
    after: Option<String>,
) -> Value {
    let mut args = Map::new();
    if let Some(c) = company_id {
        args.insert("companyId".into(), c.into());
    }
    if !types.is_empty() {
        args.insert(
            "types".into(),
            Value::Array(types.into_iter().map(Value::String).collect()),
        );
    }
    args.insert("first".into(), first.into());
    if let Some(c) = after {
        args.insert("after".into(), c.into());
    }
    Value::Object(args)
}

pub async fn content_list(
    ctx: &ToolCtx<'_>,
    company_id: Option<String>,
    types: Vec<String>,
    first: u32,
    after: Option<String>,
) -> Result<()> {
    let args = content_list_args(company_id, types, first, after);
    ctx.call("list-content", args).await
}

/// Build the `get-content` arguments — a URL goes to `url`, anything else is
/// treated as a content id.
fn content_get_args(id_or_url: &str, lang: Option<String>) -> Value {
    let mut args = Map::new();
    if id_or_url.starts_with("http://") || id_or_url.starts_with("https://") {
        args.insert("url".into(), id_or_url.into());
    } else {
        args.insert("contentId".into(), id_or_url.into());
    }
    if let Some(l) = lang {
        args.insert("lang".into(), l.into());
    }
    Value::Object(args)
}

pub async fn content_get(ctx: &ToolCtx<'_>, id_or_url: &str, lang: Option<String>) -> Result<()> {
    ctx.call("get-content", content_get_args(id_or_url, lang))
        .await
}

/// Build the `list-company-documents` arguments.
fn documents_list_args(company_id: &str, first: u32, after: Option<String>) -> Value {
    let mut args = Map::new();
    args.insert("companyId".into(), company_id.into());
    args.insert("first".into(), first.into());
    if let Some(c) = after {
        args.insert("after".into(), c.into());
    }
    Value::Object(args)
}

pub async fn documents_list(
    ctx: &ToolCtx<'_>,
    company_id: &str,
    first: u32,
    after: Option<String>,
) -> Result<()> {
    ctx.call(
        "list-company-documents",
        documents_list_args(company_id, first, after),
    )
    .await
}

pub async fn documents_get(ctx: &ToolCtx<'_>, document_id: &str) -> Result<()> {
    ctx.call("get-document", json!({ "documentId": document_id }))
        .await
}

pub async fn documents_read(
    ctx: &ToolCtx<'_>,
    document_id: &str,
    sections: Vec<u32>,
) -> Result<()> {
    ctx.call(
        "read-document-sections",
        json!({
            "documentId": document_id,
            "sectionNumbers": sections,
        }),
    )
    .await
}

// --- forum (read via the MCP server, requires login) -----------------------

pub async fn forum_search(ctx: &ToolCtx<'_>, query: &str) -> Result<()> {
    let result = ctx
        .call_raw(
            "search-forum-topics",
            json!({ "text": query, "order": "relevancy" }),
        )
        .await?;
    let sc = structured_content(&result)?;
    print_forum(&sc, ctx.json_output, forum::render_forum_search)
}

/// Read a full topic through the local SQLite cache. On each call it resumes
/// fetching from the stored pagination cursor (the end of what's cached),
/// walking forward page by page via the `get-forum-posts` tool and upserting as
/// it goes, then renders the whole thread from the cache. A mid-walk error
/// keeps progress and serves what's cached, so re-running resumes from the
/// cursor.
///
/// `--refresh` re-walks from the start (picking up edits to older posts). It
/// upserts rather than wiping first, so a refresh interrupted by a rate limit
/// or network error never destroys the previously-complete cached copy.
pub async fn forum_topic(ctx: &ToolCtx<'_>, id: &str, refresh: bool) -> Result<()> {
    let topic_id: i64 = id
        .parse()
        .map_err(|_| anyhow!("invalid topic id {id:?}: expected a number"))?;
    let cache = cache::Cache::open()?;

    let start = if refresh {
        None
    } else {
        cache.topic_cursor(topic_id)?
    };
    fetch_topic(ctx, &cache, topic_id, start).await?;

    if cache.post_count(topic_id)? == 0 {
        bail!("forum topic {id} not found or has no posts");
    }

    let envelope = json!({
        "id": topic_id,
        "title": cache.topic_title(topic_id)?.unwrap_or_default(),
        "post_stream": { "posts": cache.get_posts(topic_id)? },
    });
    print_forum(&envelope, ctx.json_output, forum::render_topic)
}

/// Walk a thread forward from `cursor` (`None` = from the start) via the
/// `get-forum-posts` tool, upserting each page into the cache and advancing the
/// stored resume cursor. Returns whether the walk was interrupted: a transient
/// error — or a deleted/private topic — stops the walk and, if posts are
/// already cached, serves them and warns rather than erroring. The walk stops
/// when the server reports no further pages (or returns an empty page, i.e.
/// we're already caught up). Shared by `forum topic` and `refresh-all`.
async fn fetch_topic(
    ctx: &ToolCtx<'_>,
    cache: &cache::Cache,
    topic_id: i64,
    cursor: Option<String>,
) -> Result<bool> {
    let mut client = ctx.client().await?;
    let mut fetcher = McpPageFetcher {
        client: &mut client,
        url: forum::thread_url(topic_id),
    };
    let result = walk_thread(cache, topic_id, cursor, &mut fetcher).await;
    let _ = client.close().await;
    result
}

/// A source of forward thread pages — the MCP `get-forum-posts` tool in
/// production, a canned list in tests. A fetch failure (transport error or an
/// `isError` result, or a deleted/private topic) is surfaced as `Err`; the walk
/// decides whether to serve cached posts or propagate.
trait PageFetcher {
    async fn fetch_page(&mut self, cursor: Option<&str>) -> Result<forum::PostsPage>;
}

/// Production [`PageFetcher`]: one `get-forum-posts` call per page.
struct McpPageFetcher<'a> {
    client: &'a mut McpClient,
    url: String,
}

impl PageFetcher for McpPageFetcher<'_> {
    async fn fetch_page(&mut self, cursor: Option<&str>) -> Result<forum::PostsPage> {
        let args = forum::page_request_args(&self.url, cursor);
        let result = self.client.call_tool("get-forum-posts", args).await?;
        if let Some(msg) = result_error_text(&result) {
            bail!("{msg}");
        }
        Ok(forum::parse_posts_page(&structured_content(&result)?))
    }
}

/// Walk a thread forward from `cursor` (`None` = from the start), upserting each
/// page into the cache and advancing the stored resume cursor. Returns whether
/// the walk was interrupted: a fetch failure stops the walk and, if posts are
/// already cached, serves them and warns (`Ok(true)`) rather than erroring. The
/// walk stops when the source reports no further pages (or returns an empty
/// page, i.e. we're already caught up). Generic over [`PageFetcher`] so the
/// pagination/resume/interrupt logic is unit-testable without a live server.
async fn walk_thread<F: PageFetcher>(
    cache: &cache::Cache,
    topic_id: i64,
    mut cursor: Option<String>,
    fetcher: &mut F,
) -> Result<bool> {
    loop {
        let page = match fetcher.fetch_page(cursor.as_deref()).await {
            Ok(page) => page,
            Err(e) => {
                if cache.post_count(topic_id)? > 0 {
                    eprintln!(
                        "warning: forum fetch for topic {topic_id} stopped ({e:#}); \
                         serving cached posts — re-run to resume."
                    );
                    return Ok(true);
                }
                return Err(e.context(format!("fetching forum topic {topic_id}")));
            }
        };
        if page.posts.is_empty() {
            // Empty thread, or resumed past the last post. For an already-cached
            // topic this is a successful "no new posts" refresh — bump synced_at
            // (preserving the cursor) so `forum topics` isn't stale.
            if cache.post_count(topic_id)? > 0 {
                cache.set_topic_meta(topic_id, None, None, cursor.as_deref())?;
            }
            return Ok(false);
        }
        cache.upsert_posts(topic_id, &page.posts)?;
        cache.set_topic_meta(
            topic_id,
            None,
            page.total_posts,
            page.next_cursor.as_deref(),
        )?;
        cursor = page.next_cursor;
        if !page.has_next {
            return Ok(false);
        }
    }
}

/// Print a forum response: raw pretty JSON under `--json`, otherwise the
/// supplied human renderer.
fn print_forum(v: &Value, as_json: bool, render: fn(&Value) -> String) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(v)?);
    } else {
        print!("{}", render(v));
    }
    Ok(())
}

/// List locally cached topics (the inventory).
pub fn forum_topics(ctx: &ToolCtx<'_>) -> Result<()> {
    if !cache::db_path()?.exists() {
        if ctx.json_output {
            println!("[]");
        } else {
            println!("No cached topics. Cache one with: inderes forum topic <id>");
        }
        return Ok(());
    }
    let cache = cache::Cache::open_readonly()?;
    let topics = cache.list_cached()?;
    if ctx.json_output {
        let arr: Vec<Value> = topics
            .iter()
            .map(|t| {
                json!({ "id": t.id, "title": t.title, "posts": t.posts, "synced_at": t.synced_at })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(arr))?);
    } else if topics.is_empty() {
        println!("No cached topics. Cache one with: inderes forum topic <id>");
    } else {
        for t in &topics {
            let title = t.title.as_deref().unwrap_or("(untitled)");
            let synced = t.synced_at.as_deref().unwrap_or("?");
            println!(
                "#{:<7} {:>6} posts  synced {synced}  {title}",
                t.id, t.posts
            );
        }
    }
    Ok(())
}

/// Remove a topic from the cache, or the whole cache with `--all`.
pub fn forum_clear(id: Option<&str>, all: bool, yes: bool) -> Result<()> {
    if !cache::db_path()?.exists() {
        println!("No cache to clear.");
        return Ok(());
    }
    let cache = cache::Cache::open()?;
    match (id, all) {
        (Some(_), true) => bail!("pass either a topic id or --all, not both"),
        (None, false) => bail!("specify a topic id, or --all to clear the whole cache"),
        (Some(id), false) => {
            let topic_id: i64 = id
                .parse()
                .map_err(|_| anyhow!("invalid topic id {id:?}: expected a number"))?;
            cache.clear_topic(topic_id)?;
            println!("Cleared topic {topic_id} from the cache.");
        }
        (None, true) => {
            if !yes && !confirm("Clear the entire forum cache?")? {
                println!("Aborted.");
                return Ok(());
            }
            cache.clear_all()?;
            println!("Cleared the entire forum cache.");
        }
    }
    Ok(())
}

/// Refresh every cached topic — pull new posts for each (incremental).
pub async fn forum_refresh_all(ctx: &ToolCtx<'_>) -> Result<()> {
    if !cache::db_path()?.exists() {
        println!("No cached topics to refresh.");
        return Ok(());
    }
    let cache = cache::Cache::open()?;
    let ids = cache.cached_topic_ids()?;
    if ids.is_empty() {
        println!("No cached topics to refresh.");
        return Ok(());
    }
    let count = ids.len();
    let mut total_new = 0i64;
    let mut incomplete = 0usize;
    for topic_id in ids {
        let before = cache.post_count(topic_id)?;
        let start = cache.topic_cursor(topic_id)?;
        match fetch_topic(ctx, &cache, topic_id, start).await {
            Ok(interrupted) => {
                let new = cache.post_count(topic_id)? - before;
                total_new += new;
                if interrupted {
                    incomplete += 1;
                }
                println!(
                    "#{topic_id}: +{new} new{}",
                    if interrupted { " (interrupted)" } else { "" }
                );
            }
            Err(e) => {
                incomplete += 1;
                eprintln!("#{topic_id}: refresh failed: {e:#}");
            }
        }
    }
    println!("Refreshed {count} topic(s), {total_new} new post(s).");
    if incomplete > 0 {
        // Non-zero exit so automation doesn't treat a partial run as current.
        bail!(
            "{incomplete} topic(s) not fully refreshed (rate limit / network) — re-run to continue"
        );
    }
    Ok(())
}

/// Prompt for y/N confirmation on stderr.
fn confirm(prompt: &str) -> Result<bool> {
    eprint!("{prompt} [y/N] ");
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Print the path to the local forum cache DB (so you can point sqlite3 /
/// datasette / duckdb / pandas at it).
pub fn forum_db_path() -> Result<()> {
    let path = cache::db_path()?;
    println!("{}", path.display());
    if !path.exists() {
        eprintln!("(no cache yet — run `inderes forum topic <id>` to create it)");
    }
    Ok(())
}

/// Run a read-only SQL query against the local forum cache. Table output by
/// default, array-of-objects under `--json`.
pub fn forum_query(ctx: &ToolCtx<'_>, sql: &str) -> Result<()> {
    let cache = cache::Cache::open_readonly()?;
    let result = cache.query(sql)?;
    if ctx.json_output {
        let cache::QueryResult { columns, rows } = result;
        let arr: Vec<Value> = rows
            .into_iter()
            .map(|row| {
                let mut obj = Map::new();
                for (col, val) in columns.iter().zip(row) {
                    // Duplicate column names (e.g. `SELECT a, b AS a`) would
                    // collapse in a JSON object — suffix the repeats so no
                    // column is silently dropped.
                    let mut key = col.clone();
                    let mut k = 2;
                    while obj.contains_key(&key) {
                        key = format!("{col}_{k}");
                        k += 1;
                    }
                    obj.insert(key, val);
                }
                Value::Object(obj)
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(arr))?);
    } else {
        print!("{}", render_query_table(&result));
        // Row count is metadata — keep it off stdout so the table pipes cleanly.
        let n = result.rows.len();
        eprintln!("({n} row{})", if n == 1 { "" } else { "s" });
    }
    Ok(())
}

/// Show posting activity over time for a cached topic — a per-bucket timeline
/// plus a momentum read (latest bucket vs the average of the others).
pub fn forum_activity(ctx: &ToolCtx<'_>, id: &str, bucket: &str, periods: u32) -> Result<()> {
    let topic_id: i64 = id
        .parse()
        .map_err(|_| anyhow!("invalid topic id {id:?}: expected a number"))?;
    let cache = cache::Cache::open_readonly()?;
    let series = cache.activity(topic_id, bucket, periods)?;
    if series.is_empty() {
        bail!("no cached posts for topic {id} — run `inderes forum topic {id}` first");
    }
    let mom = cache.momentum(topic_id, bucket, periods)?;
    if ctx.json_output {
        let mut obj = Map::new();
        obj.insert("bucket".into(), json!(bucket));
        obj.insert(
            "periods".into(),
            Value::Array(
                series
                    .iter()
                    .map(|(p, n)| json!({ "period": p, "count": n }))
                    .collect(),
            ),
        );
        if let Some((current, baseline)) = mom {
            let ratio = momentum_ratio(current, baseline);
            obj.insert(
                "momentum".into(),
                json!({
                    "current": current,
                    "baseline_avg": (baseline * 100.0).round() / 100.0,
                    "ratio": ratio_json(ratio),
                }),
            );
        }
        println!("{}", serde_json::to_string_pretty(&Value::Object(obj))?);
    } else {
        print!("{}", render_activity(topic_id, bucket, &series));
        if let Some((current, baseline)) = mom {
            let ratio = momentum_ratio(current, baseline);
            println!(
                "\nMomentum: {current} in the current {bucket} vs {baseline:.0} avg — {} ({})",
                fmt_ratio(ratio),
                momentum_label(ratio),
            );
        }
        println!("(reflects cached posts; run `inderes forum topic` to refresh)");
    }
    Ok(())
}

/// Rank cached topics by momentum — which thread is heating up most. For each
/// cached topic, compute its current-vs-baseline momentum (anchored to now, so
/// dormant threads don't replay old spikes) and sort descending. The cross-topic
/// payoff of the cache: pair with `refresh-all` to watch a whole watchlist.
pub fn forum_momentum(ctx: &ToolCtx<'_>, bucket: &str, periods: u32) -> Result<()> {
    if !cache::db_path()?.exists() {
        if ctx.json_output {
            println!("[]");
        } else {
            println!("No cached topics. Cache some with: inderes forum topic <id>");
        }
        return Ok(());
    }
    let cache = cache::Cache::open_readonly()?;
    // (id, title, current, baseline_avg, ratio)
    let mut ranked: Vec<(i64, Option<String>, i64, f64, f64)> = Vec::new();
    for t in cache.list_cached()? {
        if let Some((current, baseline)) = cache.momentum(t.id, bucket, periods)? {
            let ratio = momentum_ratio(current, baseline);
            ranked.push((t.id, t.title, current, baseline, ratio));
        }
    }
    ranked.sort_by(|a, b| b.4.total_cmp(&a.4));

    if ctx.json_output {
        let arr: Vec<Value> = ranked
            .iter()
            .map(|(id, title, current, baseline, ratio)| {
                json!({
                    "id": id, "title": title, "current": current,
                    "baseline_avg": (baseline * 100.0).round() / 100.0,
                    "ratio": ratio_json(*ratio),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(arr))?);
    } else if ranked.is_empty() {
        println!("Not enough cached history to rank momentum (need --periods 2+).");
    } else {
        println!("Cross-topic momentum (by {bucket}, current vs baseline)");
        println!("{:>6}  {:>7}  {:>5}  topic", "ratio", "current", "avg");
        for (id, title, current, baseline, ratio) in &ranked {
            let title = title.as_deref().unwrap_or("(untitled)");
            println!(
                "{:>6}  {current:>7}  {baseline:>5.0}  #{id} {title}",
                fmt_ratio(*ratio)
            );
        }
        println!(
            "\n(reflects cached posts; run `inderes forum refresh-all` for a current picture)"
        );
    }
    Ok(())
}

/// Ratio of current-bucket activity to the prior-bucket baseline. Infinite for a
/// from-nothing burst (no prior history); zero when dormant.
fn momentum_ratio(current: i64, baseline: f64) -> f64 {
    if baseline > 0.0 {
        current as f64 / baseline
    } else if current > 0 {
        f64::INFINITY
    } else {
        0.0
    }
}

fn momentum_label(ratio: f64) -> &'static str {
    if ratio >= 1.5 {
        "heating up"
    } else if ratio <= 0.66 {
        "cooling off"
    } else {
        "steady"
    }
}

/// Human-readable ratio: "1.7x", or "new" for a from-nothing burst.
fn fmt_ratio(ratio: f64) -> String {
    if ratio.is_finite() {
        format!("{ratio:.1}x")
    } else {
        "new".to_string()
    }
}

/// JSON ratio: rounded number, or null when non-finite (serde_json can't
/// represent infinity).
fn ratio_json(ratio: f64) -> Value {
    if ratio.is_finite() {
        json!((ratio * 100.0).round() / 100.0)
    } else {
        Value::Null
    }
}

/// Render the activity timeline as a small bar chart. The momentum line and
/// footer are printed by the caller (it uses the current-anchored momentum).
fn render_activity(topic_id: i64, bucket: &str, series: &[(String, i64)]) -> String {
    let mut out = format!("Activity for topic #{topic_id} (by {bucket})\n");
    let max = series.iter().map(|(_, n)| *n).max().unwrap_or(0).max(1);
    for (p, n) in series {
        let bar = "█".repeat(((*n as f64 / max as f64) * 24.0).round() as usize);
        out.push_str(&format!("{p:<10}  {n:>5}  {bar}\n"));
    }
    out
}

/// Render query results as a simple column-aligned text table.
fn render_query_table(r: &cache::QueryResult) -> String {
    if r.columns.is_empty() {
        return "(no columns)\n".to_string();
    }
    let cells: Vec<Vec<String>> = r
        .rows
        .iter()
        .map(|row| row.iter().map(cell_to_string).collect())
        .collect();
    // Every row has exactly columns.len() cells (Cache::query guarantees it),
    // so direct indexing is safe.
    let mut widths: Vec<usize> = r.columns.iter().map(|c| c.chars().count()).collect();
    for row in &cells {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count())));
    let mut out = String::new();
    let header: Vec<String> = r
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| pad(c, widths[i]))
        .collect();
    out.push_str(header.join("  ").trim_end());
    out.push('\n');
    out.push_str(
        &widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
    );
    out.push('\n');
    for row in &cells {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| pad(c, widths[i]))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

fn cell_to_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// --- generic escape hatch --------------------------------------------------

pub async fn call(
    ctx: &ToolCtx<'_>,
    tool: &str,
    kv_args: Vec<String>,
    json_args: Option<String>,
) -> Result<()> {
    let args = build_args(kv_args, json_args)?;
    ctx.call(tool, args).await
}

pub async fn call_list(ctx: &ToolCtx<'_>) -> Result<()> {
    let mut c = ctx.client().await?;
    let result = c.list_tools().await;
    let _ = c.close().await;
    let result = result?;
    if ctx.json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    // Compact human listing: name + first line of description.
    let empty = Vec::new();
    let tools = result
        .get("tools")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    for t in tools {
        let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let desc = t
            .get("description")
            .and_then(|v| v.as_str())
            .and_then(|s| s.lines().next())
            .unwrap_or("");
        println!("{name:32}  {desc}");
    }
    Ok(())
}

pub(crate) fn build_args(kv_args: Vec<String>, json_args: Option<String>) -> Result<Value> {
    if let Some(raw) = json_args {
        let v: Value =
            serde_json::from_str(&raw).with_context(|| format!("parsing --json-args: {raw}"))?;
        if !v.is_object() {
            bail!("--json-args must be a JSON object");
        }
        return Ok(v);
    }
    let mut map = Map::new();
    for entry in kv_args {
        let (k, v) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("--arg must be KEY=VALUE, got {entry:?}"))?;
        // Heuristic: try JSON first (numbers, booleans, arrays, objects,
        // quoted strings). Fall back to raw string.
        let parsed: Value = serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.into()));
        map.insert(k.to_string(), parsed);
    }
    Ok(Value::Object(map))
}

// --- skill + completions ---------------------------------------------------

pub fn install_skill(host: skill::Host, dest: Option<PathBuf>, force: bool) -> Result<PathBuf> {
    let target = dest.unwrap_or_else(|| host.default_install_path());
    let parent = target.parent().context("skill path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    if target.exists() && !force {
        bail!(
            "{} already exists — pass --force to overwrite",
            target.display()
        );
    }
    fs::write(&target, host.body()).with_context(|| format!("writing {}", target.display()))?;
    Ok(target)
}

pub fn completions(shell: Shell) -> Result<()> {
    use clap::CommandFactory;
    let mut cmd = crate::Cli::command();
    let bin = "inderes";
    clap_complete::generate(shell, &mut cmd, bin, &mut io::stdout());
    Ok(())
}

// --- upgrade / uninstall --------------------------------------------------

pub async fn upgrade(http: &reqwest::Client, check_only: bool, force: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let repo = crate::upgrade::upgrade_repo();
    let latest_tag = crate::upgrade::fetch_latest_tag(http, &repo).await?;
    let latest = latest_tag.strip_prefix('v').unwrap_or(&latest_tag);

    println!("Current version: {current}");
    println!("Latest release:  {latest_tag}");

    let newer = crate::upgrade::version_is_newer(current, latest);
    if !newer && !force {
        println!();
        println!("Already up to date.");
        return Ok(());
    }
    if check_only {
        println!();
        println!("A newer release is available. Run `inderes upgrade` to install {latest_tag}.");
        return Ok(());
    }

    let exe = std::env::current_exe().context("locating current binary")?;
    let install_dir = exe
        .parent()
        .context("current binary has no parent dir")?
        .to_path_buf();

    println!();
    println!("Upgrading via the install script.");
    println!("Install directory: {}", install_dir.display());
    println!();

    let status = run_install_script(&install_dir, &latest_tag, &repo).await?;
    if !status.success() {
        bail!("install script exited with {status}");
    }

    // The new binary is now in place. Have *it* rewrite any installed skill
    // files so an agent's on-disk SKILL.md matches the upgraded capabilities —
    // this process is still the old binary and can't emit the new skill text.
    refresh_installed_skills(&exe).await;

    println!();
    println!("Done. Verify with: inderes --version");
    Ok(())
}

/// After an upgrade, ask the freshly-installed binary to rewrite every skill
/// that's currently present at its default path, so an agent reads guidance
/// matching the new binary. Best-effort, default paths only (same scope as
/// `uninstall --remove-skills`); a custom `--dest` install isn't tracked.
async fn refresh_installed_skills(new_exe: &std::path::Path) {
    use tokio::process::Command;
    let mut refreshed_any = false;
    for host in skill::Host::ALL {
        let path = host.default_install_path();
        if !path.exists() {
            continue;
        }
        refreshed_any = true;
        let name = host.cli_name();
        match Command::new(new_exe)
            .args(["install-skill", name, "--force"])
            .status()
            .await
        {
            Ok(s) if s.success() => println!("✓ Refreshed {name} skill at {}", path.display()),
            Ok(s) => eprintln!("warning: refreshing {name} skill exited with {s}"),
            Err(e) => eprintln!("warning: could not refresh {name} skill: {e:#}"),
        }
    }
    if refreshed_any {
        println!();
    }
}

#[cfg(unix)]
async fn run_install_script(
    install_dir: &std::path::Path,
    tag: &str,
    repo: &str,
) -> Result<std::process::ExitStatus> {
    use tokio::process::Command;
    // `set -euo pipefail` is load-bearing here, not paranoia. Without
    // pipefail, `curl X | bash` returns the inner bash's exit status —
    // and an empty pipe (because curl 4xx/5xx'd) produces a no-op bash
    // that exits 0, so `inderes upgrade` would report success while
    // leaving the user on the old binary. With pipefail the pipeline's
    // status reflects whichever command in the chain failed.
    let cmd = format!(
        "set -euo pipefail; curl -fsSL https://raw.githubusercontent.com/{repo}/main/install.sh | bash"
    );
    let status = Command::new("bash")
        .arg("-c")
        .arg(&cmd)
        .env("INDERES_INSTALL_DIR", install_dir)
        .env("INDERES_VERSION", tag)
        .env("INDERES_REPO", repo)
        .status()
        .await
        .context("spawning bash to run install.sh")?;
    Ok(status)
}

#[cfg(windows)]
async fn run_install_script(
    install_dir: &std::path::Path,
    tag: &str,
    repo: &str,
) -> Result<std::process::ExitStatus> {
    use tokio::process::Command;
    // Same rationale as the Unix branch: PowerShell's default
    // `$ErrorActionPreference` is `Continue`, so a failing
    // Invoke-WebRequest just yields $null and `iex` evaluates that as
    // a no-op — exit 0, "upgrade complete", user still on old binary.
    // `Stop` makes any non-terminating cmdlet error abort the script
    // with a non-zero exit code so the failure surfaces.
    let cmd = format!(
        "$ErrorActionPreference = 'Stop'; iwr -useb https://raw.githubusercontent.com/{repo}/main/install.ps1 | iex"
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-Command", &cmd])
        .env("INDERES_INSTALL_DIR", install_dir)
        .env("INDERES_VERSION", tag)
        .env("INDERES_REPO", repo)
        .status()
        .await
        .context("spawning powershell to run install.ps1")?;
    Ok(status)
}

/// Lists the on-disk skill paths for every supported host whose skill file
/// is currently present. Pure helper for testability.
pub(crate) fn installed_skill_paths() -> Vec<PathBuf> {
    skill::Host::ALL
        .into_iter()
        .map(|h| h.default_install_path())
        .filter(|p| p.exists())
        .collect()
}

pub fn uninstall(yes: bool, remove_skills: bool) -> Result<()> {
    let exe = std::env::current_exe().context("locating current binary")?;
    let token_path = storage::token_path()?;
    let skills = if remove_skills {
        installed_skill_paths()
    } else {
        Vec::new()
    };

    println!("This will:");
    println!("  - clear stored tokens at {}", token_path.display());
    if remove_skills {
        if skills.is_empty() {
            println!("  - (no installed skill files found to remove)");
        } else {
            for s in &skills {
                println!("  - delete skill at {}", s.display());
            }
        }
    }
    println!(
        "  - print the command you should run yourself to remove the binary at {}",
        exe.display()
    );

    if !yes {
        eprint!("Continue? [y/N] ");
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }

    storage::clear()?;
    println!("✓ Tokens cleared from {}", token_path.display());

    for s in &skills {
        // Remove the SKILL.md and (best-effort) the now-empty parent dir.
        if let Err(e) = fs::remove_file(s) {
            eprintln!("  failed to remove {}: {e:#}", s.display());
        } else {
            println!("✓ Removed {}", s.display());
            if let Some(parent) = s.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }

    println!();
    println!("To complete removal, delete the binary yourself:");
    if cfg!(windows) {
        println!("  Remove-Item \"{}\"", exe.display());
    } else {
        println!("  rm {}", exe.display());
    }
    println!();
    println!("(The CLI cannot delete its own running binary cleanly across all platforms,");
    println!("so we leave that final step in your hands.)");
    Ok(())
}

// --- output helpers --------------------------------------------------------

fn print_result(result: &Value, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    render_result(result, &mut stdout)?;
    if let Some(true) = result.get("isError").and_then(|v| v.as_bool()) {
        bail!("tool returned isError=true");
    }
    Ok(())
}

/// Renders MCP `{content: [...]}` into a human-friendly stream. Text content
/// passes through verbatim; non-text content types (image/audio/resource)
/// become single-line placeholders so nobody accidentally streams binary
/// base64 to their terminal, but enough detail is preserved to identify the
/// asset and fall back to `--json` if needed.
pub(crate) fn render_result(result: &Value, out: &mut dyn io::Write) -> io::Result<()> {
    let Some(content) = result.get("content").and_then(|v| v.as_array()) else {
        writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(result).unwrap_or_default()
        )?;
        return Ok(());
    };
    if content.is_empty() {
        writeln!(out, "(empty result)")?;
        return Ok(());
    }
    for item in content {
        render_content_item(item, out)?;
    }
    Ok(())
}

fn render_content_item(item: &Value, out: &mut dyn io::Write) -> io::Result<()> {
    match item.get("type").and_then(|v| v.as_str()) {
        Some("text") => {
            if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                writeln!(out, "{t}")?;
            }
        }
        Some("image") => {
            let mime = item.get("mimeType").and_then(|v| v.as_str()).unwrap_or("?");
            let bytes_b64 = item
                .get("data")
                .and_then(|v| v.as_str())
                .map(str::len)
                .unwrap_or(0);
            writeln!(
                out,
                "[image: {mime}, {bytes_b64} bytes base64 — pass --json for raw data]"
            )?;
        }
        Some("audio") => {
            let mime = item.get("mimeType").and_then(|v| v.as_str()).unwrap_or("?");
            let bytes_b64 = item
                .get("data")
                .and_then(|v| v.as_str())
                .map(str::len)
                .unwrap_or(0);
            writeln!(
                out,
                "[audio: {mime}, {bytes_b64} bytes base64 — pass --json for raw data]"
            )?;
        }
        Some("resource") => {
            let empty = Value::Null;
            let resource = item.get("resource").unwrap_or(&empty);
            let uri = resource.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
            let mime = resource.get("mimeType").and_then(|v| v.as_str());
            if let Some(text) = resource.get("text").and_then(|v| v.as_str()) {
                writeln!(
                    out,
                    "[resource: {uri}{}]",
                    mime.map(|m| format!(", {m}")).unwrap_or_default()
                )?;
                writeln!(out, "{text}")?;
            } else if resource.get("blob").is_some() {
                writeln!(out, "[resource: {uri} (binary) — pass --json for raw data]")?;
            } else {
                writeln!(out, "[resource: {uri}]")?;
            }
        }
        Some(other) => {
            writeln!(out, "[{other} content — pass --json for raw output]")?;
        }
        None => {}
    }
    Ok(())
}

// Silence the `Ok(res)` vs `_ = res` churn — kept for future use by callers
// who build args programmatically (e.g. a REPL).
#[allow(dead_code)]
pub fn kv_to_args(pairs: &[(&str, &str)]) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), Value::String((*v).to_string()));
    }
    Value::Object(m)
}

#[allow(dead_code)]
fn _stable_map_type() -> HashMap<String, Value> {
    HashMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;

    // --- walk_thread (pagination / resume / interrupt) ---------------------

    /// A [`PageFetcher`] that hands back queued pages (or errors), recording the
    /// cursor it was asked for on each call so resume behavior can be asserted.
    struct CannedFetcher {
        pages: VecDeque<Result<forum::PostsPage>>,
        seen_cursors: Vec<Option<String>>,
    }

    impl CannedFetcher {
        fn new(pages: Vec<Result<forum::PostsPage>>) -> Self {
            Self {
                pages: pages.into_iter().collect(),
                seen_cursors: Vec::new(),
            }
        }
    }

    impl PageFetcher for CannedFetcher {
        async fn fetch_page(&mut self, cursor: Option<&str>) -> Result<forum::PostsPage> {
            self.seen_cursors.push(cursor.map(str::to_string));
            self.pages
                .pop_front()
                .expect("walk_thread requested more pages than were queued")
        }
    }

    fn page(posts: Vec<Value>, next: Option<&str>, has_next: bool) -> forum::PostsPage {
        forum::PostsPage {
            posts,
            next_cursor: next.map(str::to_string),
            has_next,
            total_posts: None,
        }
    }

    fn cache_post(id: i64, n: i64) -> Value {
        json!({"id": id, "post_number": n, "username": "u", "created_at": "2026-01-01",
               "cooked": "body", "text": "body"})
    }

    #[tokio::test]
    async fn walk_thread_caches_all_pages_until_no_next() {
        let c = cache::Cache::open_in_memory().unwrap();
        let mut f = CannedFetcher::new(vec![
            Ok(page(
                vec![cache_post(1, 1), cache_post(2, 2)],
                Some("c1"),
                true,
            )),
            Ok(page(vec![cache_post(3, 3)], Some("c2"), false)),
        ]);
        let interrupted = walk_thread(&c, 7, None, &mut f).await.unwrap();
        assert!(!interrupted);
        assert_eq!(c.post_count(7).unwrap(), 3);
        // Cursor advanced to the last page, and the second fetch resumed from
        // the first page's endCursor.
        assert_eq!(c.topic_cursor(7).unwrap().as_deref(), Some("c2"));
        assert_eq!(f.seen_cursors, vec![None, Some("c1".to_string())]);
    }

    #[tokio::test]
    async fn walk_thread_resumes_from_stored_cursor_and_marks_caught_up() {
        let c = cache::Cache::open_in_memory().unwrap();
        c.upsert_posts(7, &[cache_post(1, 1)]).unwrap();
        c.set_topic_meta(7, Some("T"), Some(1), Some("c5")).unwrap();
        // Already caught up: the resume fetch returns an empty page.
        let mut f = CannedFetcher::new(vec![Ok(page(vec![], Some("c5"), false))]);
        let interrupted = walk_thread(&c, 7, Some("c5".into()), &mut f).await.unwrap();
        assert!(!interrupted);
        assert_eq!(f.seen_cursors, vec![Some("c5".to_string())]);
        // No growth, cursor preserved (synced_at bumped — covers that branch).
        assert_eq!(c.post_count(7).unwrap(), 1);
        assert_eq!(c.topic_cursor(7).unwrap().as_deref(), Some("c5"));
    }

    #[tokio::test]
    async fn walk_thread_interrupted_serves_cached_posts() {
        let c = cache::Cache::open_in_memory().unwrap();
        c.upsert_posts(7, &[cache_post(1, 1)]).unwrap();
        let mut f = CannedFetcher::new(vec![Err(anyhow!("rate limited"))]);
        let interrupted = walk_thread(&c, 7, Some("c".into()), &mut f).await.unwrap();
        assert!(interrupted, "a failure with cached posts should serve them");
        assert_eq!(c.post_count(7).unwrap(), 1);
    }

    #[tokio::test]
    async fn walk_thread_error_without_cache_propagates() {
        let c = cache::Cache::open_in_memory().unwrap();
        let mut f = CannedFetcher::new(vec![Err(anyhow!("boom"))]);
        let err = walk_thread(&c, 7, None, &mut f).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("fetching forum topic 7"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn walk_thread_empty_first_page_leaves_topic_uncached() {
        // An empty thread (or nonexistent topic) creates no metadata row, so
        // `refresh-all` won't later try to download a phantom topic.
        let c = cache::Cache::open_in_memory().unwrap();
        let mut f = CannedFetcher::new(vec![Ok(page(vec![], None, false))]);
        let interrupted = walk_thread(&c, 7, None, &mut f).await.unwrap();
        assert!(!interrupted);
        assert_eq!(c.post_count(7).unwrap(), 0);
        assert_eq!(c.topic_cursor(7).unwrap(), None);
    }

    // --- subcommand arg builders -------------------------------------------

    #[test]
    fn fundamentals_args_includes_optional_fields_only_when_set() {
        let a = fundamentals_args(
            vec!["COMPANY:200".into()],
            "ANNUAL",
            vec!["revenue".into()],
            Some(2020),
            None,
        );
        assert_eq!(a["companyIds"], json!(["COMPANY:200"]));
        assert_eq!(a["resolution"], "ANNUAL");
        assert_eq!(a["fields"], json!(["revenue"]));
        assert_eq!(a["startYear"], 2020);
        assert!(a.get("endYear").is_none());

        // Empty fields are omitted entirely.
        let a = fundamentals_args(vec!["COMPANY:1".into()], "QUARTERLY", vec![], None, None);
        assert!(a.get("fields").is_none());
        assert!(a.get("startYear").is_none());
    }

    #[test]
    fn estimates_args_omits_empty_company_ids_but_always_sends_scalars() {
        let a = estimates_args(vec![], vec!["eps".into()], 5, true, 3);
        assert!(a.get("companyIds").is_none());
        assert_eq!(a["fields"], json!(["eps"]));
        assert_eq!(a["count"], 5);
        assert_eq!(a["includeQuarters"], true);
        assert_eq!(a["yearCount"], 3);

        let a = estimates_args(vec!["COMPANY:9".into()], vec![], 1, false, 1);
        assert_eq!(a["companyIds"], json!(["COMPANY:9"]));
        assert_eq!(a["fields"], json!([]));
    }

    #[test]
    fn content_list_args_includes_only_present_filters() {
        let a = content_list_args(Some("COMPANY:200".into()), vec!["ARTICLE".into()], 20, None);
        assert_eq!(a["companyId"], "COMPANY:200");
        assert_eq!(a["types"], json!(["ARTICLE"]));
        assert_eq!(a["first"], 20);
        assert!(a.get("after").is_none());

        let a = content_list_args(None, vec![], 10, Some("cursor123".into()));
        assert!(a.get("companyId").is_none());
        assert!(a.get("types").is_none());
        assert_eq!(a["after"], "cursor123");
    }

    #[test]
    fn content_get_args_routes_url_vs_content_id() {
        let url = content_get_args("https://www.inderes.fi/fi/article", Some("en".into()));
        assert_eq!(url["url"], "https://www.inderes.fi/fi/article");
        assert!(url.get("contentId").is_none());
        assert_eq!(url["lang"], "en");

        let id = content_get_args("ARTICLE:directus-1234", None);
        assert_eq!(id["contentId"], "ARTICLE:directus-1234");
        assert!(id.get("url").is_none());
        assert!(id.get("lang").is_none());
    }

    #[test]
    fn documents_list_args_carries_pagination() {
        let a = documents_list_args("COMPANY:200", 15, Some("c1".into()));
        assert_eq!(a["companyId"], "COMPANY:200");
        assert_eq!(a["first"], 15);
        assert_eq!(a["after"], "c1");
        assert!(documents_list_args("COMPANY:1", 5, None)
            .get("after")
            .is_none());
    }

    // --- structured_content / result_error_text ---------------------------

    #[test]
    fn structured_content_prefers_structured_field() {
        let result = json!({
            "content": [{"type": "text", "text": "{\"ignored\":true}"}],
            "structuredContent": {"posts": [], "pageInfo": {"hasNextPage": false}}
        });
        let sc = structured_content(&result).unwrap();
        assert!(sc.get("pageInfo").is_some());
        assert!(sc.get("ignored").is_none());
    }

    #[test]
    fn structured_content_falls_back_to_parsing_text() {
        let result = json!({"content": [{"type": "text", "text": "{\"topics\":[{\"id\":1}]}"}]});
        let sc = structured_content(&result).unwrap();
        assert_eq!(sc["topics"][0]["id"], 1);
    }

    #[test]
    fn structured_content_errors_without_structured_or_text() {
        let err = structured_content(&json!({"content": []})).unwrap_err();
        assert!(
            format!("{err:#}").contains("no structuredContent"),
            "got: {err:#}"
        );
    }

    #[test]
    fn result_error_text_some_only_on_is_error() {
        assert_eq!(result_error_text(&json!({"content": []})), None);
        let msg = result_error_text(&json!({
            "isError": true,
            "content": [{"type": "text", "text": "tool blew up"}]
        }));
        assert_eq!(msg.as_deref(), Some("tool blew up"));
    }

    // --- build_args --------------------------------------------------------

    #[test]
    fn build_args_parses_json_values_when_possible() {
        let args = build_args(
            vec![
                "count=10".into(),
                "enabled=true".into(),
                "tags=[\"a\",\"b\"]".into(),
                "config={\"k\":1}".into(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(args["count"], 10);
        assert_eq!(args["enabled"], true);
        assert_eq!(args["tags"], json!(["a", "b"]));
        assert_eq!(args["config"]["k"], 1);
    }

    #[test]
    fn build_args_falls_back_to_string_when_value_not_json() {
        let args = build_args(vec!["name=NOKIA".into()], None).unwrap();
        assert_eq!(args["name"], "NOKIA");
    }

    #[test]
    fn build_args_handles_values_with_equals_sign() {
        // split_once('=') should yield the first =, leaving "b=c" as value.
        let args = build_args(vec!["formula=a=b+c".into()], None).unwrap();
        assert_eq!(args["formula"], "a=b+c");
    }

    #[test]
    fn build_args_rejects_missing_equals() {
        let err = build_args(vec!["bogus".into()], None).unwrap_err();
        assert!(format!("{err:#}").contains("KEY=VALUE"));
    }

    #[test]
    fn build_args_json_args_overrides_kv_args() {
        // When --json-args is provided, KV args are ignored entirely.
        let args = build_args(
            vec!["ignored=me".into()],
            Some(r#"{"override": true}"#.into()),
        )
        .unwrap();
        assert_eq!(args, json!({"override": true}));
    }

    #[test]
    fn build_args_json_args_must_be_object() {
        let err = build_args(vec![], Some("[1,2,3]".into())).unwrap_err();
        assert!(format!("{err:#}").contains("must be a JSON object"));
    }

    #[test]
    fn build_args_json_args_rejects_invalid_json() {
        let err = build_args(vec![], Some("{bogus".into())).unwrap_err();
        assert!(format!("{err:#}").contains("parsing --json-args"));
    }

    #[test]
    fn build_args_empty_yields_empty_object() {
        let args = build_args(vec![], None).unwrap();
        assert_eq!(args, json!({}));
    }

    // --- momentum / render_activity ---------------------------------------

    #[test]
    fn momentum_ratio_handles_baseline_burst_and_dormant() {
        assert_eq!(momentum_ratio(30, 10.0), 3.0); // heating
        assert!(momentum_ratio(5, 0.0).is_infinite()); // from-nothing burst
        assert_eq!(momentum_ratio(0, 4.0), 0.0); // dormant
    }

    #[test]
    fn momentum_label_buckets_the_ratio() {
        assert_eq!(momentum_label(3.0), "heating up");
        assert_eq!(momentum_label(1.0), "steady");
        assert_eq!(momentum_label(0.4), "cooling off");
        assert_eq!(momentum_label(f64::INFINITY), "heating up");
    }

    #[test]
    fn fmt_ratio_and_ratio_json_handle_infinity() {
        assert_eq!(fmt_ratio(1.73), "1.7x");
        assert_eq!(fmt_ratio(f64::INFINITY), "new");
        assert_eq!(ratio_json(2.0), json!(2.0));
        assert_eq!(ratio_json(f64::INFINITY), Value::Null);
    }

    #[test]
    fn render_activity_shows_bars_only() {
        let s = vec![
            ("2026-W01".to_string(), 10i64),
            ("2026-W02".to_string(), 30),
        ];
        let out = render_activity(7, "week", &s);
        assert!(out.contains("topic #7"));
        assert!(out.contains("2026-W02"));
        // Momentum line is printed by the caller, not the chart renderer.
        assert!(!out.contains("Momentum"), "got: {out}");
    }

    // --- render_query_table -----------------------------------------------

    #[test]
    fn render_query_table_aligns_columns() {
        let r = cache::QueryResult {
            columns: vec!["username".into(), "n".into()],
            rows: vec![
                vec![json!("alice"), json!(2)],
                vec![json!("bob"), json!(10)],
            ],
        };
        let out = render_query_table(&r);
        assert!(out.contains("username"));
        assert!(out.contains("alice"));
        assert!(out.contains("bob"));
        assert!(out.contains("10"));
        // The row count is emitted on stderr by forum_query, not in the table.
        assert!(!out.contains("rows)"), "got: {out}");
    }

    #[test]
    fn render_query_table_handles_null_cell() {
        let r = cache::QueryResult {
            columns: vec!["a".into()],
            rows: vec![vec![Value::Null]],
        };
        let out = render_query_table(&r);
        assert!(out.starts_with("a\n"), "got: {out}"); // header present, null renders blank
    }

    // --- render_result / render_content_item ------------------------------

    fn render(result: &Value) -> String {
        let mut buf = Vec::new();
        render_result(result, &mut buf).expect("render");
        String::from_utf8(buf).expect("utf8")
    }

    #[test]
    fn render_text_content_emits_body() {
        let out = render(&json!({
            "content": [{"type": "text", "text": "hello world"}]
        }));
        assert_eq!(out.trim(), "hello world");
    }

    #[test]
    fn render_multiple_text_items_prints_each() {
        let out = render(&json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ]
        }));
        assert!(out.contains("first"));
        assert!(out.contains("second"));
    }

    #[test]
    fn render_image_content_prints_placeholder_with_mime_and_size() {
        let b64 = "AAAA".repeat(1000); // 4000 chars
        let out = render(&json!({
            "content": [{"type": "image", "mimeType": "image/png", "data": b64}]
        }));
        assert!(out.contains("[image:"));
        assert!(out.contains("image/png"));
        assert!(out.contains("4000 bytes"));
        // Critical: raw base64 must NOT reach stdout.
        assert!(!out.contains("AAAA"));
    }

    #[test]
    fn render_audio_content_prints_placeholder() {
        let out = render(&json!({
            "content": [{"type": "audio", "mimeType": "audio/wav", "data": "XXXX"}]
        }));
        assert!(out.contains("[audio:"));
        assert!(out.contains("audio/wav"));
    }

    #[test]
    fn render_resource_with_text_inlines_content() {
        let out = render(&json!({
            "content": [{
                "type": "resource",
                "resource": {
                    "uri": "file:///tmp/x.txt",
                    "mimeType": "text/plain",
                    "text": "body contents"
                }
            }]
        }));
        assert!(out.contains("[resource: file:///tmp/x.txt"));
        assert!(out.contains("text/plain"));
        assert!(out.contains("body contents"));
    }

    #[test]
    fn render_resource_with_blob_prints_binary_placeholder() {
        let out = render(&json!({
            "content": [{
                "type": "resource",
                "resource": {
                    "uri": "file:///tmp/x.bin",
                    "blob": "BASE64DATA"
                }
            }]
        }));
        assert!(out.contains("[resource: file:///tmp/x.bin (binary)"));
        assert!(!out.contains("BASE64DATA"));
    }

    #[test]
    fn render_unknown_content_type_prints_passthrough_hint() {
        let out = render(&json!({
            "content": [{"type": "future-type", "whatever": 42}]
        }));
        assert!(out.contains("[future-type content"));
        assert!(out.contains("--json"));
    }

    #[test]
    fn render_empty_content_array_prints_placeholder() {
        let out = render(&json!({"content": []}));
        assert_eq!(out.trim(), "(empty result)");
    }

    #[test]
    fn render_without_content_key_falls_back_to_pretty_json() {
        let out = render(&json!({"foo": "bar"}));
        assert!(out.contains("\"foo\""));
        assert!(out.contains("\"bar\""));
    }

    // --- print_result wrapper (isError branch) -----------------------------

    #[test]
    fn print_result_surfaces_is_error() {
        // Non-JSON path: prints content, then bails with the isError signal.
        // The "tool said no" body lands on stdout as a side effect, which
        // cargo captures under --show-output only.
        let err = print_result(
            &json!({
                "content": [{"type": "text", "text": "tool said no"}],
                "isError": true
            }),
            false,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("isError=true"));
    }

    #[test]
    fn print_result_json_mode_does_not_consult_is_error() {
        // --json preserves the raw MCP shape; user decides how to interpret.
        // isError=true with --json should NOT terminate the process.
        let res = print_result(
            &json!({
                "content": [{"type": "text", "text": "ok"}],
                "isError": true
            }),
            true,
        );
        assert!(res.is_ok(), "--json should not fail on isError");
    }
}
