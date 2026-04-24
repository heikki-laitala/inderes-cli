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

pub const CLIENT_ID: &str = "inderes-mcp";
pub const AUTH_URL: &str =
    "https://sso.inderes.fi/auth/realms/Inderes/protocol/openid-connect/auth";
pub const TOKEN_URL: &str =
    "https://sso.inderes.fi/auth/realms/Inderes/protocol/openid-connect/token";
pub const USERINFO_URL: &str =
    "https://sso.inderes.fi/auth/realms/Inderes/protocol/openid-connect/userinfo";
pub const DEFAULT_SCOPES: &[&str] = &["openid", "offline_access", "profile", "email"];

/// Run the interactive login flow, returning freshly-minted tokens.
pub async fn login(http: &reqwest::Client, scopes: &[&str], open_browser: bool) -> Result<Tokens> {
    let verifier = random_verifier();
    let challenge = pkce_s256(&verifier);
    let state = random_urlsafe(24);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding loopback listener for OAuth callback")?;
    let addr: SocketAddr = listener.local_addr()?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", addr.port());

    let auth_url = build_auth_url(&redirect_uri, &challenge, &state, scopes)?;

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

    let tokens = exchange_code(http, &code, &redirect_uri, &verifier).await?;
    Ok(tokens)
}

/// Refresh an access token using the stored refresh token. Returns new tokens
/// (the refresh token itself may rotate).
pub async fn refresh(http: &reqwest::Client, refresh_token: &str) -> Result<Tokens> {
    let params = [
        ("grant_type", "refresh_token"),
        ("client_id", CLIENT_ID),
        ("refresh_token", refresh_token),
    ];
    post_token(http, &params).await
}

/// Call Keycloak `/userinfo` with the current access token. Useful for
/// `inderes whoami`.
pub async fn userinfo(http: &reqwest::Client, access_token: &str) -> Result<serde_json::Value> {
    let resp = http
        .get(USERINFO_URL)
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
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
    scopes: &[&str],
) -> Result<Url> {
    let mut u = Url::parse(AUTH_URL)?;
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", &scopes.join(" "))
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(u)
}

async fn exchange_code(
    http: &reqwest::Client,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Tokens> {
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    post_token(http, &params).await
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

async fn post_token(http: &reqwest::Client, params: &[(&str, &str)]) -> Result<Tokens> {
    let resp = http
        .post(TOKEN_URL)
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
    rand::thread_rng().fill(&mut buf[..]);
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
