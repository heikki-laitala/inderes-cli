//! Integration tests that invoke the `inderes` binary as a subprocess.
//! Exercises the clap dispatch in `main.rs` — the part `cargo test --lib`
//! can't reach — and verifies the no-auth command paths stay functional.
//!
//! Every test redirects HOME / APPDATA / XDG_* to a fresh tempdir so the
//! runner's real token file (if any) is never touched.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn isolated() -> (Command, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let mut cmd = Command::cargo_bin("inderes").expect("cargo bin built");
    // Redirect every config-dir root the `directories` crate consults on
    // any of our supported platforms so nothing escapes into a real user
    // profile.
    cmd.env("HOME", tmp.path());
    cmd.env("XDG_CONFIG_HOME", tmp.path().join(".config"));
    cmd.env("XDG_DATA_HOME", tmp.path().join(".local/share"));
    cmd.env("APPDATA", tmp.path().join("AppData/Roaming"));
    cmd.env("LOCALAPPDATA", tmp.path().join("AppData/Local"));
    // Don't let ambient INDERES_* env bleed in.
    cmd.env_remove("INDERES_MCP_ENDPOINT");
    cmd.env_remove("INDERES_IDP_AUTH_URL");
    cmd.env_remove("INDERES_IDP_TOKEN_URL");
    cmd.env_remove("INDERES_IDP_USERINFO_URL");
    cmd.env_remove("INDERES_IDP_CLIENT_ID");
    cmd.env_remove("INDERES_FORUM_DB");
    (cmd, tmp)
}

// --- metadata ----------------------------------------------------------

#[test]
fn version_prints_crate_version() {
    let (mut cmd, _tmp) = isolated();
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("inderes "));
}

#[test]
fn help_lists_all_subcommands() {
    let (mut cmd, _tmp) = isolated();
    let expected = [
        "login",
        "logout",
        "whoami",
        "search",
        "fundamentals",
        "estimates",
        "content",
        "documents",
        "forum",
        "call",
        "install-skill",
        "completions",
    ];
    let mut assertion = cmd.arg("--help").assert().success();
    for sub in expected {
        assertion = assertion.stdout(predicate::str::contains(sub));
    }
}

#[test]
fn forum_help_lists_subcommands() {
    let (mut cmd, _tmp) = isolated();
    let mut assertion = cmd.args(["forum", "--help"]).assert().success();
    for sub in ["search", "topic", "query", "momentum", "refresh-all"] {
        assertion = assertion.stdout(predicate::str::contains(sub));
    }
}

// --- no-auth paths -----------------------------------------------------

#[test]
fn logout_with_no_stored_tokens_succeeds() {
    let (mut cmd, _tmp) = isolated();
    cmd.arg("logout")
        .assert()
        .success()
        .stdout(predicate::str::contains("Signed out"));
}

#[test]
fn whoami_with_no_tokens_reports_not_signed_in() {
    let (mut cmd, _tmp) = isolated();
    cmd.arg("whoami")
        .assert()
        .success()
        .stdout(predicate::str::contains("Not signed in"));
}

#[test]
fn completions_generates_shell_script() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let (mut cmd, _tmp) = isolated();
        cmd.args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("inderes").and(predicate::str::is_empty().not()));
    }
}

// --- install-skill -----------------------------------------------------

#[test]
fn install_skill_writes_each_host() {
    for host in ["openclaw", "hermes", "ptrclaw"] {
        let (mut cmd, tmp) = isolated();
        let dest = tmp.path().join(format!("{host}-SKILL.md"));
        cmd.args(["install-skill", host, "--dest", dest.to_str().unwrap()])
            .assert()
            .success()
            .stdout(predicate::str::contains("Skill written to"));

        let body = std::fs::read_to_string(&dest).expect("skill file");
        assert!(body.contains("name: inderes"));
        assert!(
            body.len() > 500,
            "suspiciously short skill body: {}",
            body.len()
        );
    }
}

#[test]
fn install_skill_refuses_to_overwrite_without_force() {
    let (mut cmd, tmp) = isolated();
    let dest = tmp.path().join("preexisting.md");
    std::fs::write(&dest, "don't clobber me").unwrap();

    cmd.args([
        "install-skill",
        "openclaw",
        "--dest",
        dest.to_str().unwrap(),
    ])
    .assert()
    .failure()
    .stderr(predicate::str::contains("already exists"));

    // Existing content must be untouched.
    let body = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(body, "don't clobber me");
}

#[test]
fn install_skill_force_overwrites() {
    let (mut cmd, tmp) = isolated();
    let dest = tmp.path().join("exists.md");
    std::fs::write(&dest, "old contents").unwrap();

    cmd.args([
        "install-skill",
        "openclaw",
        "--force",
        "--dest",
        dest.to_str().unwrap(),
    ])
    .assert()
    .success();

    let body = std::fs::read_to_string(&dest).unwrap();
    assert!(body.contains("name: inderes"));
    assert!(!body.contains("old contents"));
}

#[test]
fn install_skill_rejects_unknown_host() {
    let (mut cmd, _tmp) = isolated();
    cmd.args(["install-skill", "bogus-host"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("'bogus-host'").or(predicate::str::contains("bogus-host")),
        );
}

// --- auth-required commands fail cleanly ------------------------------

#[test]
fn search_without_login_prints_helpful_error() {
    let (mut cmd, _tmp) = isolated();
    cmd.args(["search", "NOKIA"]).assert().failure().stderr(
        predicate::str::contains("not signed in").and(predicate::str::contains("inderes login")),
    );
}

#[test]
fn call_list_without_login_prints_helpful_error() {
    let (mut cmd, _tmp) = isolated();
    cmd.args(["call", "--list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not signed in"));
}

// --- uninstall ---------------------------------------------------------

#[test]
fn uninstall_yes_clears_tokens_and_prints_rm_hint() {
    let (mut cmd, _tmp) = isolated();
    cmd.args(["uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Tokens cleared"))
        .stdout(predicate::str::contains("delete the binary yourself"));
}

#[test]
fn uninstall_with_remove_skills_succeeds_when_none_installed() {
    let (mut cmd, _tmp) = isolated();
    cmd.args(["uninstall", "--yes", "--remove-skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no installed skill files"));
}

// Unix-only: this test pre-seeds a skill at $HOME/.openclaw/... and relies on
// the CLI resolving its install path off the same $HOME override. The
// `directories` crate uses SHGetKnownFolderPath on Windows and ignores
// $HOME / $USERPROFILE — so the override doesn't reach the CLI's path
// resolver, and the pre-seeded file lives at a different place than the CLI
// looks. The skill-removal logic itself is platform-agnostic and covered by
// `uninstall_with_remove_skills_succeeds_when_none_installed` on every OS.
#[cfg(unix)]
#[test]
fn uninstall_actually_removes_skill_files_when_present() {
    let (mut cmd, tmp) = isolated();
    let skill = tmp.path().join(".openclaw/skills/inderes/SKILL.md");
    std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
    std::fs::write(&skill, "name: inderes\n---\nbody").unwrap();

    cmd.args(["uninstall", "--yes", "--remove-skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed"));

    assert!(!skill.exists(), "skill file should have been removed");
}

// --- upgrade ----------------------------------------------------------

#[test]
fn upgrade_help_lists_check_only_and_force() {
    let (mut cmd, _tmp) = isolated();
    cmd.args(["upgrade", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--check-only"))
        .stdout(predicate::str::contains("--force"));
}

#[test]
fn upgrade_against_nonexistent_repo_surfaces_error() {
    let (mut cmd, _tmp) = isolated();
    // Direct upgrade at a repo that 404s. We don't need a mock server
    // because GitHub's API will return a real 404 for nonsense paths.
    cmd.env(
        "INDERES_REPO",
        "definitely-does-not-exist-12345/inderes-cli-test-please-ignore",
    )
    .args(["upgrade", "--check-only"])
    .assert()
    .failure()
    .stderr(predicate::str::contains("404").or(predicate::str::contains("error")));
}

// --- end-to-end subcommand dispatch with mocked MCP server ---------------
//
// These spin up an in-process MCP mock and pre-seed a tokens.json via the
// INDERES_TOKEN_PATH override so subcommands that normally need auth can
// run without the real Keycloak dance. Exercises the full happy-path
// chain: storage → auth → McpClient → subcommand arg building → MCP call
// → result rendering.

mod mocked {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::method as wm_method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Write a tokens.json good for the next hour at the given path.
    fn seed_valid_tokens(path: &std::path::Path) {
        let body = json!({
            "access_token": "fake-access-token",
            "refresh_token": null,
            "expires_at": (time::OffsetDateTime::now_utc() + time::Duration::minutes(60))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
            "token_type": "Bearer",
            "scope": "openid offline_access"
        });
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body.to_string()).unwrap();
    }

    fn cmd_with_mcp(tmp: &TempDir, server: &MockServer) -> Command {
        let token_file = tmp.path().join("tokens.json");
        seed_valid_tokens(&token_file);
        let mut cmd = Command::cargo_bin("inderes").unwrap();
        cmd.env("HOME", tmp.path())
            .env("XDG_CONFIG_HOME", tmp.path().join(".config"))
            .env("INDERES_TOKEN_PATH", &token_file)
            .env("INDERES_MCP_ENDPOINT", server.uri());
        cmd
    }

    /// Mock the `initialize` + `notifications/initialized` preamble. The
    /// response advertises our pinned protocol version so the version-drift
    /// warning doesn't fire in test output.
    async fn mount_init(server: &MockServer) {
        Mock::given(wm_method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "sess-int")
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "protocolVersion": "2025-03-26",
                            "serverInfo": {"name": "mock", "version": "0.0.1"}
                        }
                    })),
            )
            .up_to_n_times(1)
            .mount(server)
            .await;
        // notifications/initialized + any DELETE on close — catch-all so the
        // specific tool-call mock (mounted below) runs for the third POST.
        Mock::given(wm_method("POST"))
            .respond_with(ResponseTemplate::new(202))
            .up_to_n_times(1)
            .mount(server)
            .await;
    }

    async fn mount_tool_response(server: &MockServer, id: i64, result: serde_json::Value) {
        Mock::given(wm_method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result
                    })),
            )
            .up_to_n_times(1)
            .mount(server)
            .await;
        // DELETE on close — best-effort, accept anything.
        Mock::given(wm_method("DELETE"))
            .respond_with(ResponseTemplate::new(204))
            .mount(server)
            .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_runs_end_to_end_against_mcp() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({
                "content": [{"type": "text", "text": "COMPANY:200 Nokia Corp"}]
            }),
        )
        .await;

        let mut cmd = cmd_with_mcp(&tmp, &server);
        cmd.args(["search", "Nokia"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Nokia Corp"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn call_list_runs_end_to_end_against_mcp() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({
                "tools": [
                    {"name": "search-companies", "description": "Search by name"},
                    {"name": "get-fundamentals", "description": "Historical financials"}
                ]
            }),
        )
        .await;

        let mut cmd = cmd_with_mcp(&tmp, &server);
        cmd.args(["call", "--list"])
            .assert()
            .success()
            .stdout(predicate::str::contains("search-companies"))
            .stdout(predicate::str::contains("get-fundamentals"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn call_generic_passes_kv_args_through() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({
                "content": [{"type": "text", "text": "ok"}]
            }),
        )
        .await;

        let mut cmd = cmd_with_mcp(&tmp, &server);
        cmd.args([
            "call",
            "search-companies",
            "--arg",
            "query=Nokia",
            "--arg",
            "limit=5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ok"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fundamentals_sends_structured_args() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({
                "content": [{"type": "text", "text": "revenue 1000, ebitda 200"}]
            }),
        )
        .await;

        let mut cmd = cmd_with_mcp(&tmp, &server);
        cmd.args([
            "fundamentals",
            "COMPANY:200",
            "--field",
            "revenue",
            "--field",
            "ebitda",
            "--from-year",
            "2022",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("revenue 1000"));
    }

    fn cmd_with_mcp_and_db(tmp: &TempDir, server: &MockServer, db: &std::path::Path) -> Command {
        let mut cmd = cmd_with_mcp(tmp, server);
        cmd.env("INDERES_FORUM_DB", db);
        cmd
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn estimates_runs_end_to_end_against_mcp() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({"content": [{"type": "text", "text": "EPS 2026e: 0.42"}]}),
        )
        .await;

        cmd_with_mcp(&tmp, &server)
            .args(["estimates", "COMPANY:200", "--field", "eps", "--count", "3"])
            .assert()
            .success()
            .stdout(predicate::str::contains("EPS 2026e"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn content_list_and_get_run_end_to_end() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({"content": [{"type": "text", "text": "ARTICLE:directus-1 Outlook raised"}]}),
        )
        .await;
        cmd_with_mcp(&tmp, &server)
            .args(["content", "list", "--type", "ARTICLE"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Outlook raised"));

        // A fresh server for the second invocation (mocks are one-shot).
        let server2 = MockServer::start().await;
        mount_init(&server2).await;
        mount_tool_response(
            &server2,
            2,
            json!({"content": [{"type": "text", "text": "# Body markdown"}]}),
        )
        .await;
        cmd_with_mcp(&tmp, &server2)
            .args([
                "content",
                "get",
                "https://www.inderes.fi/fi/x",
                "--lang",
                "en",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("Body markdown"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn documents_read_runs_end_to_end() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({"content": [{"type": "text", "text": "Section 3: Risks"}]}),
        )
        .await;
        cmd_with_mcp(&tmp, &server)
            .args(["documents", "read", "DOCUMENT:1", "--sections", "3"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Section 3"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forum_search_renders_topic_ids_from_mcp() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({
                "structuredContent": {"topics": [
                    {"title": "Nokia sijoituskohteena (Osa 4)", "postsCount": 1176,
                     "url": "https://forum.inderes.com/t/nokia-osa-4/73687"}
                ]},
                "content": [{"type": "text", "text": "ignored"}]
            }),
        )
        .await;
        cmd_with_mcp(&tmp, &server)
            .args(["forum", "search", "Nokia"])
            .assert()
            .success()
            .stdout(predicate::str::contains("#73687"))
            .stdout(predicate::str::contains("1176 posts"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forum_topic_caches_then_cache_commands_read_it() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("forum.db");
        let server = MockServer::start().await;
        mount_init(&server).await;
        // One page, no next page: caches the posts and stops.
        mount_tool_response(
            &server,
            2,
            json!({
                "structuredContent": {
                    "posts": [
                        {"id": 1, "postNumber": 1, "username": "alice",
                         "createdAt": "2026-01-02T00:00:00Z", "content": "Bullish",
                         "url": "https://forum.inderes.com/t/-/74/1", "score": 5, "replyCount": 0},
                        {"id": 2, "postNumber": 2, "username": "bob",
                         "createdAt": "2026-01-03T00:00:00Z", "content": "Bearish",
                         "url": "https://forum.inderes.com/t/-/74/2", "score": 1, "replyCount": 0}
                    ],
                    "pageInfo": {"endCursor": "2", "hasNextPage": false, "totalPosts": 2}
                }
            }),
        )
        .await;

        // 1) Fetch the thread → caches it and renders the bodies.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "topic", "74"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Bullish"))
            .stdout(predicate::str::contains("@bob"));

        // 2) `forum topics` lists the cached inventory (no server needed).
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "topics"])
            .assert()
            .success()
            .stdout(predicate::str::contains("#74"))
            .stdout(predicate::str::contains("2 posts"));

        // 3) `forum query` runs read-only SQL over the cached posts.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args([
                "forum",
                "query",
                "SELECT username FROM posts ORDER BY post_number",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("alice"))
            .stdout(predicate::str::contains("bob"));

        // 4) `forum momentum` ranks cached topics (deterministic, no model).
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "momentum"])
            .assert()
            .success()
            .stdout(predicate::str::contains("#74"));

        // 5) `forum activity` buckets the cached posts over time.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "activity", "74", "--bucket", "month"])
            .assert()
            .success();

        // 5b) `--json` variants exercise the structured-output branches.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["--json", "forum", "query", "SELECT username FROM posts"])
            .assert()
            .success()
            .stdout(predicate::str::contains("\"username\""));
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["--json", "forum", "activity", "74", "--bucket", "month"])
            .assert()
            .success()
            .stdout(predicate::str::contains("\"periods\""));

        // 6) `forum db-path` prints the cache location.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "db-path"])
            .assert()
            .success()
            .stdout(predicate::str::contains("forum.db"));

        // 7) `forum refresh-all` re-walks each cached topic; a fresh server
        //    serves an empty page (already caught up) → no new posts.
        let server2 = MockServer::start().await;
        mount_init(&server2).await;
        mount_tool_response(
            &server2,
            2,
            json!({"structuredContent": {"posts": [], "pageInfo": {"hasNextPage": false}}}),
        )
        .await;
        cmd_with_mcp_and_db(&tmp, &server2, &db)
            .args(["forum", "refresh-all"])
            .assert()
            .success()
            .stdout(predicate::str::contains("+0 new"));

        // 8) `forum clear` argument validation (cache exists, so these reach
        //    the conflict/empty-arg branches rather than the no-cache early-out).
        cmd_with_mcp_and_db(&tmp, &server2, &db)
            .args(["forum", "clear", "74", "--all"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("not both"));
        cmd_with_mcp_and_db(&tmp, &server2, &db)
            .args(["forum", "clear"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("specify a topic id"));

        // 9) `forum clear <id>` drops one topic; `--all --yes` wipes the rest.
        cmd_with_mcp_and_db(&tmp, &server2, &db)
            .args(["forum", "clear", "74"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Cleared topic 74"));
        cmd_with_mcp_and_db(&tmp, &server2, &db)
            .args(["forum", "clear", "--all", "--yes"])
            .assert()
            .success();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forum_topic_with_no_posts_reports_not_found() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("forum.db");
        let server = MockServer::start().await;
        mount_init(&server).await;
        // Empty thread (or deleted/private topic): no posts, nothing cached.
        mount_tool_response(
            &server,
            2,
            json!({"structuredContent": {"posts": [], "pageInfo": {"hasNextPage": false}}}),
        )
        .await;
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "topic", "999999"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("not found or has no posts"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forum_cache_commands_are_graceful_without_a_cache() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("missing.db");
        let server = MockServer::start().await;

        // Inventory / momentum report "no cached topics" rather than erroring.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "topics"])
            .assert()
            .success()
            .stdout(predicate::str::contains("No cached topics"));
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "momentum"])
            .assert()
            .success()
            .stdout(predicate::str::contains("No cached topics"));
        // --json variants emit an empty array.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["--json", "forum", "topics"])
            .assert()
            .success()
            .stdout(predicate::str::contains("[]"));

        // db-path prints the location plus a "no cache yet" hint on stderr.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "db-path"])
            .assert()
            .success()
            .stderr(predicate::str::contains("no cache yet"));

        // query/activity need a populated cache and say so.
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "query", "SELECT 1"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("no forum cache"));
        cmd_with_mcp_and_db(&tmp, &server, &db)
            .args(["forum", "activity", "1"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("no forum cache"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn whoami_with_valid_token_reports_signed_in() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        // whoami (non-verbose) reads the stored token's expiry and makes no
        // network call; seed a valid token and expect a signed-in message.
        cmd_with_mcp(&tmp, &server)
            .arg("whoami")
            .assert()
            .success()
            .stdout(predicate::str::contains("Signed in"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn json_flag_emits_raw_tool_result() {
        let tmp = TempDir::new().unwrap();
        let server = MockServer::start().await;
        mount_init(&server).await;
        mount_tool_response(
            &server,
            2,
            json!({
                "content": [{"type": "text", "text": "ignored"}],
                "isError": false,
                "meta": {"server": "mock"}
            }),
        )
        .await;

        let mut cmd = cmd_with_mcp(&tmp, &server);
        cmd.args(["--json", "search", "Nokia"])
            .assert()
            .success()
            // --json dumps the full MCP result object, including non-content
            // fields (meta) that the human formatter would drop.
            .stdout(predicate::str::contains("\"meta\""))
            .stdout(predicate::str::contains("\"server\": \"mock\""));
    }
}
