//! OAuth 2.1 authorization-code + PKCE flow against the Inderes Keycloak
//! realm, plus refresh-token handling.
//!
//! Flow:
//! 1. Start a loopback HTTP listener on an ephemeral 127.0.0.1 port.
//! 2. Build the Keycloak `/auth` URL with `code_challenge=S256(verifier)` and
//!    a random `state`.
//! 3. Open the URL in the user's default browser.
//! 4. Keycloak redirects the browser to `http://127.0.0.1:<port>/callback`
//!    with `code` + `state`. We answer 200 with a plain "you can close this
//!    tab" page and shut down the listener.
//! 5. POST `grant_type=authorization_code` to Keycloak's token endpoint
//!    (public client -> no `client_secret`, just the verifier).
//!
//! Refresh is similar but simpler — POST `grant_type=refresh_token`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use time::{Duration as TimeDuration, OffsetDateTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};
use url::Url;

use crate::storage::Tokens;

const DEFAULT_CLIENT_ID: &str = "inderes-mcp";
const DEFAULT_AUTH_URL: &str =
    "https://sso.inderes.fi/auth/realms/Inderes/protocol/openid-connect/auth";
const DEFAULT_TOKEN_URL: &str =
    "https://sso.inderes.fi/auth/realms/Inderes/protocol/openid-connect/token";
const DEFAULT_USERINFO_URL: &str =
    "https://sso.inderes.fi/auth/realms/Inderes/protocol/openid-connect/userinfo";
pub const DEFAULT_SCOPES: &[&str] = &["openid", "offline_access", "profile", "email"];

/// All Keycloak endpoints + client info in one place, so pointing at a
/// staging realm or a self-hosted IdP only needs a few env vars instead of
/// a recompile.
#[derive(Debug, Clone)]
pub struct IdpConfig {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub userinfo_endpoint: String,
    pub client_id: String,
}

impl IdpConfig {
    /// The baked-in Inderes production realm.
    pub fn inderes_default() -> Self {
        Self {
            authorization_endpoint: DEFAULT_AUTH_URL.into(),
            token_endpoint: DEFAULT_TOKEN_URL.into(),
            userinfo_endpoint: DEFAULT_USERINFO_URL.into(),
            client_id: DEFAULT_CLIENT_ID.into(),
        }
    }

    /// Start from the Inderes defaults, then apply any env-var overrides.
    pub fn from_env() -> Self {
        let mut cfg = Self::inderes_default();
        if let Ok(v) = std::env::var("INDERES_IDP_AUTH_URL") {
            cfg.authorization_endpoint = v;
        }
        if let Ok(v) = std::env::var("INDERES_IDP_TOKEN_URL") {
            cfg.token_endpoint = v;
        }
        if let Ok(v) = std::env::var("INDERES_IDP_USERINFO_URL") {
            cfg.userinfo_endpoint = v;
        }
        if let Ok(v) = std::env::var("INDERES_IDP_CLIENT_ID") {
            cfg.client_id = v;
        }
        cfg
    }
}

/// Run the interactive login flow, returning freshly-minted tokens.
pub async fn login(
    http: &reqwest::Client,
    idp: &IdpConfig,
    scopes: &[&str],
    open_browser: bool,
) -> Result<Tokens> {
    let verifier = random_verifier();
    let challenge = pkce_s256(&verifier);
    let state = random_urlsafe(24);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding loopback listener for OAuth callback")?;
    let addr: SocketAddr = listener.local_addr()?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", addr.port());

    let auth_url = build_auth_url(idp, &redirect_uri, &challenge, &state, scopes)?;

    eprintln!("Opening browser to sign in with your Inderes account…");
    eprintln!("If the browser does not open, visit this URL:\n  {auth_url}");
    if open_browser {
        let _ = webbrowser::open(auth_url.as_str());
    }

    let (code_tx, code_rx) = oneshot::channel::<Result<String, String>>();
    let expected_state = state.clone();

    tokio::spawn(async move {
        if let Err(e) = serve_one_callback(listener, expected_state, code_tx).await {
            eprintln!("callback listener error: {e:#}");
        }
    });

    // Five-minute cap — Keycloak auth codes expire faster than this but a
    // generous outer limit prevents the CLI from hanging forever.
    let code = match timeout(Duration::from_secs(300), code_rx).await {
        Ok(Ok(Ok(code))) => code,
        Ok(Ok(Err(msg))) => bail!("login failed: {msg}"),
        Ok(Err(_)) => bail!("login aborted: callback channel closed"),
        Err(_) => bail!("login timed out after 5 minutes"),
    };

    let tokens = exchange_code(http, idp, &code, &redirect_uri, &verifier).await?;
    Ok(tokens)
}

/// Refresh an access token using the stored refresh token. Returns new tokens
/// (the refresh token itself may rotate).
pub async fn refresh(
    http: &reqwest::Client,
    idp: &IdpConfig,
    refresh_token: &str,
) -> Result<Tokens> {
    let params = [
        ("grant_type", "refresh_token"),
        ("client_id", idp.client_id.as_str()),
        ("refresh_token", refresh_token),
    ];
    post_token(http, idp, &params).await
}

/// Call Keycloak `/userinfo` with the current access token. Useful for
/// `inderes whoami`.
pub async fn userinfo(
    http: &reqwest::Client,
    idp: &IdpConfig,
    access_token: &str,
) -> Result<serde_json::Value> {
    let resp = http
        .get(&idp.userinfo_endpoint)
        .bearer_auth(access_token)
        .send()
        .await
        .context("calling Keycloak userinfo")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("userinfo returned {status}: {body}");
    }
    Ok(serde_json::from_str(&body)?)
}

// --- internals --------------------------------------------------------------

fn build_auth_url(
    idp: &IdpConfig,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
    scopes: &[&str],
) -> Result<Url> {
    let mut u = Url::parse(&idp.authorization_endpoint)?;
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &idp.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(u)
}

async fn exchange_code(
    http: &reqwest::Client,
    idp: &IdpConfig,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Tokens> {
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", idp.client_id.as_str()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    post_token(http, idp, &params).await
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

async fn post_token(
    http: &reqwest::Client,
    idp: &IdpConfig,
    params: &[(&str, &str)],
) -> Result<Tokens> {
    let resp = http
        .post(&idp.token_endpoint)
        .form(params)
        .send()
        .await
        .context("POST token endpoint")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let parsed: TokenResponse = serde_json::from_str(&body)
        .with_context(|| format!("parsing token response ({status}): {body}"))?;

    if let Some(err) = parsed.error {
        let desc = parsed.error_description.unwrap_or_default();
        bail!("Keycloak {err}: {desc}");
    }

    let expires_in = parsed.expires_in.unwrap_or(300);
    let expires_at = OffsetDateTime::now_utc() + TimeDuration::seconds(expires_in);

    Ok(Tokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at,
        token_type: parsed.token_type,
        scope: parsed.scope,
    })
}

// --- PKCE + random helpers --------------------------------------------------

fn random_verifier() -> String {
    // RFC 7636: 43-128 unreserved chars. 64 bytes of entropy is comfortable.
    random_urlsafe(64)
}

fn random_urlsafe(n_bytes: usize) -> String {
    let mut buf = vec![0u8; n_bytes];
    rand::rng().fill(&mut buf[..]);
    BASE64_URL_SAFE_NO_PAD.encode(&buf)
}

fn pkce_s256(verifier: &str) -> String {
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    BASE64_URL_SAFE_NO_PAD.encode(h.finalize())
}

// --- callback server --------------------------------------------------------

// Accept exactly one connection, parse the first request line, and respond
// with a small HTML page. We do not bring in a web framework for this.
async fn serve_one_callback(
    listener: TcpListener,
    expected_state: String,
    code_tx: oneshot::Sender<Result<String, String>>,
) -> Result<()> {
    let code_tx = Arc::new(std::sync::Mutex::new(Some(code_tx)));
    loop {
        let (mut sock, _) = listener.accept().await?;

        let mut buf = [0u8; 8192];
        let n = match timeout(Duration::from_secs(30), sock.read(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => 0,
        };
        let req = String::from_utf8_lossy(&buf[..n]).to_string();

        // First line: `GET /callback?code=...&state=... HTTP/1.1`
        let path = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");

        let result: Result<String, String> = parse_callback_path(path, &expected_state);

        let (status, body) = match &result {
            Ok(_) => ("200 OK", CALLBACK_SUCCESS_HTML),
            Err(e) => {
                eprintln!("OAuth callback error: {e}");
                ("400 Bad Request", CALLBACK_ERROR_HTML)
            }
        };

        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.shutdown().await;

        let mut guard = code_tx.lock().map_err(|_| anyhow!("mutex poisoned"))?;
        if let Some(tx) = guard.take() {
            let _ = tx.send(result);
            return Ok(());
        }
    }
}

fn parse_callback_path(path: &str, expected_state: &str) -> Result<String, String> {
    if !path.starts_with("/callback") {
        return Err(format!("unexpected callback path: {path}"));
    }
    // `Url::parse` needs an absolute URL — prepend a dummy origin.
    let url = Url::parse(&format!("http://127.0.0.1{path}"))
        .map_err(|e| format!("unparseable callback URL: {e}"))?;

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut err: Option<String> = None;
    let mut err_desc: Option<String> = None;

    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => err = Some(v.into_owned()),
            "error_description" => err_desc = Some(v.into_owned()),
            _ => {}
        }
    }

    if let Some(e) = err {
        return Err(format!(
            "authorization server returned {e}: {}",
            err_desc.as_deref().unwrap_or("")
        ));
    }
    let state = state.ok_or_else(|| "missing `state` in callback".to_string())?;
    if state != expected_state {
        return Err("state mismatch — possible CSRF; aborting".into());
    }
    code.ok_or_else(|| "missing `code` in callback".into())
}

const CALLBACK_SUCCESS_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>inderes-cli — signed in</title></head><body style="font-family:-apple-system,system-ui,sans-serif;max-width:28rem;margin:4rem auto;text-align:center"><h1>Signed in</h1><p>You can close this tab and return to the terminal.</p></body></html>"#;

const CALLBACK_ERROR_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>inderes-cli — error</title></head><body style="font-family:-apple-system,system-ui,sans-serif;max-width:28rem;margin:4rem auto;text-align:center"><h1>Sign-in failed</h1><p>Check the terminal for details.</p></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // --- Pure helpers ------------------------------------------------------

    #[test]
    fn pkce_s256_matches_rfc_fixture() {
        // Test vector from RFC 7636 §4.5. Verifier is 43 ASCII chars; the
        // known-good challenge is "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM".
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = pkce_s256(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn random_verifier_is_rfc_compliant_length() {
        // RFC 7636 requires 43..=128 unreserved chars. Base64url of 64 bytes
        // gives 86 chars — comfortably inside the range.
        let v = random_verifier();
        assert!(v.len() >= 43 && v.len() <= 128, "got {}", v.len());
        // URL-safe no-pad uses only [A-Za-z0-9_-].
        assert!(
            v.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "non-urlsafe chars in {v}"
        );
    }

    #[test]
    fn random_urlsafe_length_varies_with_input() {
        // 32 bytes → 43 base64url chars (no padding).
        assert_eq!(random_urlsafe(32).len(), 43);
        // 24 bytes → 32 base64url chars.
        assert_eq!(random_urlsafe(24).len(), 32);
    }

    // --- IdpConfig ---------------------------------------------------------

    #[test]
    fn idp_default_points_at_inderes() {
        let cfg = IdpConfig::inderes_default();
        assert_eq!(cfg.client_id, "inderes-mcp");
        assert!(cfg.authorization_endpoint.contains("sso.inderes.fi"));
        assert!(cfg.token_endpoint.ends_with("/token"));
        assert!(cfg.userinfo_endpoint.ends_with("/userinfo"));
    }

    #[test]
    fn idp_from_env_layers_overrides() {
        // SAFETY: setting env vars in tests is only race-safe if no other
        // test in this module also mutates the same vars. We isolate by
        // using unique var names — but the `from_env()` fn reads fixed names,
        // so we do one sequential mutation set + restore.
        let keys = [
            ("INDERES_IDP_AUTH_URL", "https://staging/auth"),
            ("INDERES_IDP_TOKEN_URL", "https://staging/token"),
            ("INDERES_IDP_USERINFO_URL", "https://staging/userinfo"),
            ("INDERES_IDP_CLIENT_ID", "staging-client"),
        ];
        for (k, v) in keys {
            // SAFETY: tests run serially by default when they touch env.
            unsafe { std::env::set_var(k, v) };
        }
        let cfg = IdpConfig::from_env();
        assert_eq!(cfg.authorization_endpoint, "https://staging/auth");
        assert_eq!(cfg.token_endpoint, "https://staging/token");
        assert_eq!(cfg.userinfo_endpoint, "https://staging/userinfo");
        assert_eq!(cfg.client_id, "staging-client");
        for (k, _) in keys {
            unsafe { std::env::remove_var(k) };
        }
    }

    // --- build_auth_url ----------------------------------------------------

    #[test]
    fn build_auth_url_attaches_pkce_and_scopes() {
        let cfg = IdpConfig::inderes_default();
        let url = build_auth_url(
            &cfg,
            "http://127.0.0.1:12345/callback",
            "challenge-xyz",
            "state-abc",
            &["openid", "offline_access"],
        )
        .unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(q.get("client_id").map(String::as_str), Some("inderes-mcp"));
        assert_eq!(
            q.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:12345/callback")
        );
        assert_eq!(q.get("state").map(String::as_str), Some("state-abc"));
        assert_eq!(
            q.get("code_challenge").map(String::as_str),
            Some("challenge-xyz")
        );
        assert_eq!(
            q.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        // scopes are joined with a space.
        assert_eq!(
            q.get("scope").map(String::as_str),
            Some("openid offline_access")
        );
    }

    // --- parse_callback_path ----------------------------------------------

    #[test]
    fn parse_callback_happy_path_returns_code() {
        let code = parse_callback_path("/callback?code=abc123&state=s1", "s1").unwrap();
        assert_eq!(code, "abc123");
    }

    #[test]
    fn parse_callback_rejects_state_mismatch_as_csrf() {
        let err = parse_callback_path("/callback?code=abc&state=bad", "good").unwrap_err();
        assert!(err.contains("state mismatch"), "got: {err}");
    }

    #[test]
    fn parse_callback_rejects_missing_state() {
        let err = parse_callback_path("/callback?code=abc", "s1").unwrap_err();
        assert!(err.contains("missing `state`"), "got: {err}");
    }

    #[test]
    fn parse_callback_rejects_wrong_path() {
        let err = parse_callback_path("/elsewhere?code=x", "s").unwrap_err();
        assert!(err.contains("unexpected callback path"), "got: {err}");
    }

    #[test]
    fn parse_callback_surfaces_error_param() {
        let err = parse_callback_path(
            "/callback?error=access_denied&error_description=User+declined&state=s1",
            "s1",
        )
        .unwrap_err();
        assert!(err.contains("access_denied"), "got: {err}");
        assert!(err.contains("User declined"), "got: {err}");
    }

    #[test]
    fn parse_callback_rejects_missing_code() {
        let err = parse_callback_path("/callback?state=s1", "s1").unwrap_err();
        assert!(err.contains("missing `code`"), "got: {err}");
    }

    // --- Token endpoint (wiremock) ----------------------------------------

    fn test_idp(server: &MockServer) -> IdpConfig {
        IdpConfig {
            authorization_endpoint: format!("{}/auth", server.uri()),
            token_endpoint: format!("{}/token", server.uri()),
            userinfo_endpoint: format!("{}/userinfo", server.uri()),
            client_id: "test-client".into(),
        }
    }

    #[tokio::test]
    async fn refresh_parses_happy_response() {
        let server = MockServer::start().await;
        let idp = test_idp(&server);

        Mock::given(wm_method("POST"))
            .and(wm_path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("client_id=test-client"))
            .and(body_string_contains("refresh_token=rt-old"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "new-access",
                "refresh_token": "rt-new",
                "expires_in": 300,
                "token_type": "Bearer",
                "scope": "openid offline_access"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let tokens = refresh(&http, &idp, "rt-old").await.unwrap();
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("rt-new"));
        assert_eq!(tokens.token_type.as_deref(), Some("Bearer"));
        // Expiry should be ~300s in the future.
        let remaining = (tokens.expires_at - OffsetDateTime::now_utc()).whole_seconds();
        assert!(
            (290..=310).contains(&remaining),
            "expected ~300s, got {remaining}"
        );
    }

    #[tokio::test]
    async fn refresh_surfaces_keycloak_error() {
        let server = MockServer::start().await;
        let idp = test_idp(&server);

        Mock::given(wm_method("POST"))
            .and(wm_path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "invalid_grant",
                "error_description": "Refresh token expired"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = refresh(&http, &idp, "rt-expired").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("invalid_grant"), "got: {msg}");
        assert!(msg.contains("Refresh token expired"), "got: {msg}");
    }

    #[tokio::test]
    async fn refresh_handles_missing_expires_in() {
        // Some IdPs omit expires_in; we fall back to 300s.
        let server = MockServer::start().await;
        let idp = test_idp(&server);

        Mock::given(wm_method("POST"))
            .and(wm_path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "a",
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let tokens = refresh(&http, &idp, "r").await.unwrap();
        let remaining = (tokens.expires_at - OffsetDateTime::now_utc()).whole_seconds();
        assert!(
            (290..=310).contains(&remaining),
            "expected default ~300s, got {remaining}"
        );
    }

    #[tokio::test]
    async fn exchange_code_posts_authorization_code_grant() {
        let server = MockServer::start().await;
        let idp = test_idp(&server);

        Mock::given(wm_method("POST"))
            .and(wm_path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=auth-code"))
            .and(body_string_contains("code_verifier=verif-xyz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "a",
                "refresh_token": "r",
                "expires_in": 600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let tokens = exchange_code(
            &http,
            &idp,
            "auth-code",
            "http://127.0.0.1:12345/callback",
            "verif-xyz",
        )
        .await
        .unwrap();
        assert_eq!(tokens.access_token, "a");
    }

    // --- userinfo (wiremock) ----------------------------------------------

    #[tokio::test]
    async fn userinfo_returns_claims_on_200() {
        let server = MockServer::start().await;
        let idp = test_idp(&server);

        Mock::given(wm_method("GET"))
            .and(wm_path("/userinfo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sub": "user-123",
                "preferred_username": "alice",
                "email": "alice@example.com"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let claims = userinfo(&http, &idp, "access-token").await.unwrap();
        assert_eq!(claims["preferred_username"], "alice");
        assert_eq!(claims["email"], "alice@example.com");
    }

    #[tokio::test]
    async fn userinfo_bubbles_401() {
        let server = MockServer::start().await;
        let idp = test_idp(&server);

        Mock::given(wm_method("GET"))
            .and(wm_path("/userinfo"))
            .respond_with(ResponseTemplate::new(401).set_body_string("expired"))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = userinfo(&http, &idp, "expired-token").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("401"), "got: {msg}");
    }
}
