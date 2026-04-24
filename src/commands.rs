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
use crate::mcp::McpClient;
use crate::oauth;
use crate::skill;
use crate::storage;

// --- login / logout / whoami ------------------------------------------------

pub async fn login(http: &reqwest::Client, no_browser: bool) -> Result<()> {
    let tokens = oauth::login(http, oauth::DEFAULT_SCOPES, !no_browser).await?;
    storage::save(&tokens)?;
    let ui = oauth::userinfo(http, &tokens.access_token).await.ok();
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

pub async fn whoami(http: &reqwest::Client, verbose: bool) -> Result<()> {
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
        match oauth::userinfo(http, &tokens.access_token).await {
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
    pub json_output: bool,
}

impl<'a> ToolCtx<'a> {
    async fn client(&self) -> Result<McpClient> {
        let token = auth::ensure_access_token(self.http).await?;
        let mut c = McpClient::new(self.http.clone(), self.endpoint, token);
        c.initialize().await.context("initializing MCP session")?;
        Ok(c)
    }

    async fn call(&self, tool: &str, args: Value) -> Result<()> {
        let mut c = self.client().await?;
        let result = c.call_tool(tool, args).await?;
        print_result(&result, self.json_output)
    }
}

pub async fn search(ctx: &ToolCtx<'_>, query: &str) -> Result<()> {
    ctx.call("search-companies", json!({ "query": query }))
        .await
}

pub async fn fundamentals(
    ctx: &ToolCtx<'_>,
    company_ids: Vec<String>,
    resolution: &str,
    fields: Vec<String>,
    start_year: Option<i32>,
    end_year: Option<i32>,
) -> Result<()> {
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
    ctx.call("get-fundamentals", Value::Object(args)).await
}

pub async fn estimates(
    ctx: &ToolCtx<'_>,
    company_ids: Vec<String>,
    fields: Vec<String>,
    count: u32,
    quarters: bool,
    year_count: u32,
) -> Result<()> {
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
    ctx.call("get-inderes-estimates", Value::Object(args)).await
}

pub async fn content_list(
    ctx: &ToolCtx<'_>,
    company_id: Option<String>,
    types: Vec<String>,
    first: u32,
    after: Option<String>,
) -> Result<()> {
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
    ctx.call("list-content", Value::Object(args)).await
}

pub async fn content_get(ctx: &ToolCtx<'_>, id_or_url: &str, lang: Option<String>) -> Result<()> {
    let mut args = Map::new();
    if id_or_url.starts_with("http://") || id_or_url.starts_with("https://") {
        args.insert("url".into(), id_or_url.into());
    } else {
        args.insert("contentId".into(), id_or_url.into());
    }
    if let Some(l) = lang {
        args.insert("lang".into(), l.into());
    }
    ctx.call("get-content", Value::Object(args)).await
}

pub async fn documents_list(
    ctx: &ToolCtx<'_>,
    company_id: &str,
    first: u32,
    after: Option<String>,
) -> Result<()> {
    let mut args = Map::new();
    args.insert("companyId".into(), company_id.into());
    args.insert("first".into(), first.into());
    if let Some(c) = after {
        args.insert("after".into(), c.into());
    }
    ctx.call("list-company-documents", Value::Object(args))
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
    let result = c.list_tools().await?;
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

fn build_args(kv_args: Vec<String>, json_args: Option<String>) -> Result<Value> {
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

// --- output helpers --------------------------------------------------------

fn print_result(result: &Value, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }
    // MCP tool results typically look like:
    //   { "content": [ { "type": "text", "text": "..." }, ... ], "isError": false }
    // Surface the concatenated `text` bodies; fall back to JSON on anything
    // we don't recognize.
    if let Some(content) = result.get("content").and_then(|v| v.as_array()) {
        let mut printed_any = false;
        for item in content {
            match item.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        println!("{t}");
                        printed_any = true;
                    }
                }
                Some(other) => {
                    eprintln!("<{other} content omitted; re-run with --json to inspect>");
                }
                None => {}
            }
        }
        if !printed_any && content.is_empty() {
            println!("(empty result)");
        }
        if let Some(true) = result.get("isError").and_then(|v| v.as_bool()) {
            bail!("tool returned isError=true");
        }
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(result)?);
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
