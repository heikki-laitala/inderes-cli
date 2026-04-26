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
    println!();
    println!("Done. Verify with: inderes --version");
    Ok(())
}

#[cfg(unix)]
async fn run_install_script(
    install_dir: &std::path::Path,
    tag: &str,
    repo: &str,
) -> Result<std::process::ExitStatus> {
    use tokio::process::Command;
    let cmd = format!("curl -fsSL https://raw.githubusercontent.com/{repo}/main/install.sh | bash");
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
    let cmd = format!("iwr -useb https://raw.githubusercontent.com/{repo}/main/install.ps1 | iex");
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
    [
        skill::Host::Openclaw,
        skill::Host::Hermes,
        skill::Host::Ptrclaw,
    ]
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
