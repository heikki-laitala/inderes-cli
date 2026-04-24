//! Minimal MCP client speaking the 2025-03-26 "Streamable HTTP" transport.
//!
//! Per the spec, the client POSTs a single JSON-RPC request to the server.
//! The server may answer with:
//!   - `Content-Type: application/json`  → one JSON-RPC response, or
//!   - `Content-Type: text/event-stream` → zero or more SSE `data:` frames,
//!     each a JSON-RPC message.
//!
//! For a one-shot CLI we just need: initialize → initialized notification →
//! one request (e.g. `tools/list` or `tools/call`) → done. Session IDs are
//! honoured if the server issues them.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::StatusCode;
use serde_json::{json, Value};

pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const CLIENT_NAME: &str = "inderes-cli";
const SESSION_HEADER: &str = "Mcp-Session-Id";
const PROTOCOL_HEADER: &str = "MCP-Protocol-Version";

/// Max number of attempts including the initial one. Three retries on top
/// of the original request is enough to ride out a short-lived upstream
/// blip without masking a real outage.
const MAX_ATTEMPTS: u32 = 4;

// Production backoff; tests override to nanoseconds so they stay fast.
#[cfg(not(test))]
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const MAX_BACKOFF: Duration = Duration::from_millis(4000);
#[cfg(test)]
const INITIAL_BACKOFF: Duration = Duration::from_millis(1);
#[cfg(test)]
const MAX_BACKOFF: Duration = Duration::from_millis(4);

/// HTTP statuses worth retrying: rate-limit + canonical transient gateway
/// errors. 500 is deliberately excluded — a 500 from an MCP server usually
/// means application-level badness that a retry won't fix.
fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

pub struct McpClient {
    http: reqwest::Client,
    endpoint: String,
    access_token: String,
    session_id: Option<String>,
    next_id: i64,
}

impl McpClient {
    pub fn new(
        http: reqwest::Client,
        endpoint: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
            access_token: access_token.into(),
            session_id: None,
            next_id: 1,
        }
    }

    /// Send `initialize` + `notifications/initialized`. Must be called once
    /// before any other request.
    pub async fn initialize(&mut self) -> Result<Value> {
        let params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": CLIENT_NAME,
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        let server_info = self.request("initialize", params).await?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(server_info)
    }

    /// Call a tool and return its `result` object. The CLI surfaces this
    /// directly as JSON (or a human summary).
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let params = json!({
            "name": name,
            "arguments": arguments,
        });
        self.request("tools/call", params).await
    }

    /// Return `tools/list` verbatim.
    pub async fn list_tools(&mut self) -> Result<Value> {
        self.request("tools/list", json!({})).await
    }

    /// Send `DELETE /` with the `Mcp-Session-Id` header to let the server
    /// release session resources. Best-effort: the 2025-03-26 spec says
    /// clients SHOULD do this on shutdown but servers may also return 404
    /// if they've already evicted the session, which we treat as success.
    pub async fn close(&mut self) -> Result<()> {
        let Some(sid) = self.session_id.take() else {
            return Ok(());
        };
        let resp = self
            .http
            .delete(&self.endpoint)
            .bearer_auth(&self.access_token)
            .header(SESSION_HEADER, sid)
            .send()
            .await
            .context("DELETE MCP session")?;
        let status = resp.status();
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            bail!("DELETE MCP session returned {status}")
        }
    }

    // --- internals ----------------------------------------------------------

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let resp = self.post(&body).await?;
        let status = resp.status();

        // Capture session ID if server issued one on this response.
        if let Some(sid) = resp
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            if self.session_id.as_deref() != Some(sid) {
                self.session_id = Some(sid.to_string());
            }
        }

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("MCP {method} returned {status}: {body}");
        }

        if content_type.starts_with("text/event-stream") {
            read_sse_result(resp, id).await
        } else {
            let v: Value = resp.json().await.context("parsing JSON-RPC response")?;
            extract_result(v, id)
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let resp = self.post(&body).await?;
        // Notifications should yield 202 Accepted, but we accept any 2xx.
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("MCP notify {method} returned {code}: {body}");
        }
        Ok(())
    }

    /// POST with exponential backoff on transient failures. Retriable cases:
    /// transport-level errors (connect/TLS/DNS) from `reqwest::send()`, and
    /// HTTP statuses in `is_retryable_status`. Non-retriable statuses
    /// (4xx except 429, 5xx except 502/503/504) return immediately so we
    /// don't mask real errors.
    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
        let mut delay = INITIAL_BACKOFF;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.post_once(body).await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() || !is_retryable_status(status) {
                        return Ok(resp);
                    }
                    tracing::warn!(
                        "MCP {status} on attempt {attempt}/{MAX_ATTEMPTS}; retrying in {:?}",
                        delay
                    );
                    last_err = Some(anyhow!("MCP upstream returned {status}"));
                }
                Err(e) => {
                    tracing::warn!(
                        "MCP transport error on attempt {attempt}/{MAX_ATTEMPTS}: {e:#}"
                    );
                    last_err = Some(e);
                }
            }
            if attempt < MAX_ATTEMPTS {
                tokio::time::sleep(delay).await;
                delay = std::cmp::min(delay * 2, MAX_BACKOFF);
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("MCP request failed after {MAX_ATTEMPTS} attempts")))
    }

    async fn post_once(&self, body: &Value) -> Result<reqwest::Response> {
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            PROTOCOL_HEADER,
            HeaderValue::from_static(MCP_PROTOCOL_VERSION),
        );
        if let Some(sid) = &self.session_id {
            headers.insert(SESSION_HEADER, HeaderValue::from_str(sid)?);
        }

        self.http
            .post(&self.endpoint)
            .bearer_auth(&self.access_token)
            .headers(headers)
            .json(body)
            .send()
            .await
            .context("POST MCP endpoint")
    }
}

fn extract_result(msg: Value, expected_id: i64) -> Result<Value> {
    if let Some(err) = msg.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
        bail!("MCP error {code}: {message}");
    }
    let id = msg.get("id").and_then(|v| v.as_i64());
    if id != Some(expected_id) {
        // The server may send unrelated notifications on the stream first;
        // only called via `extract_result` for non-streamed responses where
        // ID must match.
        bail!("MCP id mismatch: expected {expected_id}, got {id:?}");
    }
    msg.get("result")
        .cloned()
        .ok_or_else(|| anyhow!("MCP response missing `result`: {msg}"))
}

async fn read_sse_result(resp: reqwest::Response, expected_id: i64) -> Result<Value> {
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading SSE chunk")?;
        buf.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(idx) = find_event_boundary(&buf) {
            let (event, rest) = buf.split_at(idx);
            let event = event.to_string();
            buf = rest.trim_start_matches(['\n', '\r']).to_string();
            if let Some(msg) = parse_sse_event(&event)? {
                if msg.get("method").is_some() && msg.get("id").is_none() {
                    // Server notification — ignore, keep reading.
                    continue;
                }
                if msg.get("id").and_then(|v| v.as_i64()) == Some(expected_id) {
                    return extract_result(msg, expected_id);
                }
                // Unrelated response; keep reading.
            }
        }
    }
    bail!("SSE stream ended without a response for id {expected_id}");
}

fn find_event_boundary(buf: &str) -> Option<usize> {
    // Prefer the 4-byte CRLF form first; otherwise the bare LF form. Picking
    // the *first* 4-byte boundary would still be correct in any mixed input
    // because `\r\n\r\n` contains no embedded `\n\n`, but being explicit
    // about the two forms keeps the split arithmetic obviously right: return
    // the index **past** the boundary.
    if let Some(i) = buf.find("\r\n\r\n") {
        let lf_idx = buf.find("\n\n").unwrap_or(usize::MAX);
        if lf_idx < i {
            Some(lf_idx + 2)
        } else {
            Some(i + 4)
        }
    } else {
        buf.find("\n\n").map(|i| i + 2)
    }
}

fn parse_sse_event(event: &str) -> Result<Option<Value>> {
    let mut data_lines: Vec<&str> = Vec::new();
    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
        // `event:`, `id:`, `retry:`, and `:comment` lines are ignored — we
        // only care about the accumulated `data:` payload.
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let joined = data_lines.join("\n");
    let v: Value = serde_json::from_str(&joined)
        .with_context(|| format!("parsing SSE data as JSON: {joined}"))?;
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- find_event_boundary ------------------------------------------------

    #[test]
    fn boundary_lf_only() {
        // "a\n\nb" — boundary is "\n\n" at index 1, consumer should start at 3.
        assert_eq!(find_event_boundary("a\n\nb"), Some(3));
    }

    #[test]
    fn boundary_crlf() {
        // "a\r\n\r\nb" — boundary is the 4-byte CRLF pair at index 1, start at 5.
        assert_eq!(find_event_boundary("a\r\n\r\nb"), Some(5));
    }

    #[test]
    fn boundary_none_when_incomplete() {
        assert_eq!(find_event_boundary(""), None);
        assert_eq!(find_event_boundary("hello"), None);
        assert_eq!(find_event_boundary("one\nline"), None);
        assert_eq!(find_event_boundary("half\r\n"), None);
    }

    #[test]
    fn boundary_lf_wins_when_earlier() {
        // "a\n\nbbb\r\n\r\n" — LF pair at index 1 beats CRLF pair at index 6.
        assert_eq!(find_event_boundary("a\n\nbbb\r\n\r\n"), Some(3));
    }

    #[test]
    fn boundary_consumer_can_split_correctly() {
        // Integration-style: after split_at(idx), the rest should begin with
        // the next event (not leftover boundary bytes).
        let buf = "data: one\r\n\r\ndata: two\r\n\r\n";
        let idx = find_event_boundary(buf).expect("boundary");
        let (event, rest) = buf.split_at(idx);
        assert_eq!(event, "data: one\r\n\r\n");
        assert_eq!(rest, "data: two\r\n\r\n");
    }

    // --- parse_sse_event ----------------------------------------------------

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse_sse_event("").unwrap().is_none());
        assert!(parse_sse_event("event: message\n").unwrap().is_none());
        assert!(parse_sse_event(": keep-alive comment\n").unwrap().is_none());
    }

    #[test]
    fn parse_single_line_data() {
        let v = parse_sse_event("data: {\"id\":1,\"result\":\"ok\"}")
            .unwrap()
            .unwrap();
        assert_eq!(v.get("id").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(v.get("result").and_then(|v| v.as_str()), Some("ok"));
    }

    #[test]
    fn parse_strips_single_leading_space_only() {
        // SSE spec: exactly one leading space after "data:" is a delimiter.
        // Additional spaces survive as payload — here the second space lands
        // INSIDE the JSON string literal so it reaches the parsed value.
        let v = parse_sse_event("data:  \" preserved\"").unwrap().unwrap();
        assert_eq!(v.as_str(), Some(" preserved"));
    }

    #[test]
    fn parse_accepts_no_space_after_colon() {
        // `data:foo` (no space) is valid per spec — the entire rest is payload.
        let v = parse_sse_event("data:\"tight\"").unwrap().unwrap();
        assert_eq!(v.as_str(), Some("tight"));
    }

    #[test]
    fn parse_multiline_data_joins_with_newlines() {
        // Per spec, multiple `data:` lines in one event are concatenated with
        // a `\n` between them.
        let event = "data: {\ndata:   \"id\": 1\ndata: }";
        let v = parse_sse_event(event).unwrap().unwrap();
        assert_eq!(v["id"], 1);
    }

    #[test]
    fn parse_ignores_unknown_fields() {
        let event = "event: message\nid: 42\nretry: 100\n: comment\ndata: {\"hello\":\"world\"}";
        let v = parse_sse_event(event).unwrap().unwrap();
        assert_eq!(v["hello"], "world");
    }

    #[test]
    fn parse_reports_invalid_json() {
        let err = parse_sse_event("data: {not json").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("parsing SSE data as JSON"), "got: {msg}");
    }

    // --- end-to-end SSE frame handling --------------------------------------
    //
    // Drives find_event_boundary + parse_sse_event together on a realistic
    // MCP-shaped payload containing one server notification followed by the
    // expected response. The notification (method + no id) should be
    // skipped; the response should be returned.

    #[test]
    fn chunked_stream_skips_notifications_and_returns_response() {
        let mut buf = String::new();
        // Chunks the server might have flushed separately:
        let chunks = [
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n\n",
        ];
        let expected_id: i64 = 1;
        let mut found: Option<Value> = None;

        for chunk in chunks {
            buf.push_str(chunk);
            while let Some(idx) = find_event_boundary(&buf) {
                let (event, rest) = buf.split_at(idx);
                let event_owned = event.to_string();
                buf = rest.trim_start_matches(['\n', '\r']).to_string();

                if let Some(msg) = parse_sse_event(&event_owned).unwrap() {
                    if msg.get("method").is_some() && msg.get("id").is_none() {
                        continue; // server notification — ignore
                    }
                    if msg.get("id").and_then(|v| v.as_i64()) == Some(expected_id) {
                        found = Some(msg);
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }

        let msg = found.expect("response with id=1 should be found");
        let result = extract_result(msg, expected_id).unwrap();
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "hi");
    }

    // --- McpClient integration tests (wiremock) -----------------------------
    //
    // These spin up an in-process HTTP server and exercise the full
    // request/response path: header construction, session-id capture,
    // retry-on-5xx, content-type branching, DELETE-on-close.

    use serde_json::json;
    use wiremock::matchers::{header, header_exists, method as wm_method};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    /// Convenience: build a fresh reqwest client + MockServer + McpClient.
    async fn fixture() -> (MockServer, McpClient) {
        let server = MockServer::start().await;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let client = McpClient::new(http, server.uri(), "test-token");
        (server, client)
    }

    /// `wiremock::matchers` doesn't ship a "body contains this JSON-RPC
    /// method" matcher; write a tiny one.
    struct MethodEq(&'static str);
    impl Match for MethodEq {
        fn matches(&self, req: &Request) -> bool {
            serde_json::from_slice::<Value>(&req.body)
                .ok()
                .and_then(|v| v.get("method").and_then(|m| m.as_str()).map(String::from))
                .is_some_and(|m| m == self.0)
        }
    }

    // --- initialize + session tracking --------------------------------------

    #[tokio::test]
    async fn initialize_captures_session_id_from_response_header() {
        let (server, mut client) = fixture().await;

        Mock::given(wm_method("POST"))
            .and(MethodEq("initialize"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "sess-abc123")
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {"serverInfo": {"name": "mock", "version": "0.1"}}
                    })),
            )
            .mount(&server)
            .await;

        Mock::given(wm_method("POST"))
            .and(MethodEq("notifications/initialized"))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;

        client.initialize().await.unwrap();
        assert_eq!(client.session_id.as_deref(), Some("sess-abc123"));
    }

    #[tokio::test]
    async fn subsequent_requests_echo_session_id_header() {
        let (server, mut client) = fixture().await;

        // First call: server issues a session id.
        Mock::given(wm_method("POST"))
            .and(MethodEq("initialize"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "sess-xyz")
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({"jsonrpc":"2.0","id":1,"result":{}})),
            )
            .mount(&server)
            .await;
        Mock::given(wm_method("POST"))
            .and(MethodEq("notifications/initialized"))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;

        // Second call (tools/list) must include the Mcp-Session-Id header.
        Mock::given(wm_method("POST"))
            .and(MethodEq("tools/list"))
            .and(header("Mcp-Session-Id", "sess-xyz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({"jsonrpc":"2.0","id":2,"result":{"tools":[]}})),
            )
            .mount(&server)
            .await;

        client.initialize().await.unwrap();
        let result = client.list_tools().await.unwrap();
        assert_eq!(result["tools"], json!([]));
    }

    // --- call_tool / list_tools JSON path -----------------------------------

    #[tokio::test]
    async fn call_tool_returns_result_from_json_response() {
        let (server, mut client) = fixture().await;
        client.session_id = Some("sess".into()); // skip init

        Mock::given(wm_method("POST"))
            .and(MethodEq("tools/call"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {"content": [{"type":"text","text":"pong"}]}
                    })),
            )
            .mount(&server)
            .await;

        let out = client
            .call_tool("ping", json!({"a": 1}))
            .await
            .expect("call_tool");
        assert_eq!(out["content"][0]["text"], "pong");
    }

    #[tokio::test]
    async fn id_mismatch_surfaces_as_error() {
        let (server, mut client) = fixture().await;

        Mock::given(wm_method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({"jsonrpc":"2.0","id":999,"result":{}})),
            )
            .mount(&server)
            .await;

        let err = client.list_tools().await.unwrap_err();
        assert!(
            format!("{err:#}").contains("MCP id mismatch"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn error_object_surfaces_as_error() {
        let (server, mut client) = fixture().await;

        Mock::given(wm_method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "error": {"code": -32601, "message": "Method not found"}
                    })),
            )
            .mount(&server)
            .await;

        let err = client.list_tools().await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("-32601"), "got: {msg}");
        assert!(msg.contains("Method not found"), "got: {msg}");
    }

    // --- SSE response path --------------------------------------------------

    #[tokio::test]
    async fn sse_content_type_parsed_correctly() {
        let (server, mut client) = fixture().await;

        let sse_body = concat!(
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"ping\"}]}}\n\n",
        );

        Mock::given(wm_method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse_body, "text/event-stream"),
            )
            .mount(&server)
            .await;

        let result = client.list_tools().await.unwrap();
        assert_eq!(result["tools"][0]["name"], "ping");
    }

    // --- retry behaviour ----------------------------------------------------

    #[tokio::test]
    async fn retries_on_503_then_succeeds() {
        let (server, mut client) = fixture().await;

        // First: 503 twice, then success. wiremock serves matchers in
        // registration order; a bounded matcher is used first, then the
        // catch-all.
        Mock::given(wm_method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(wm_method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/json")
                    .set_body_json(json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}})),
            )
            .mount(&server)
            .await;

        let out = client.list_tools().await.unwrap();
        assert_eq!(out["ok"], true);
    }

    #[tokio::test]
    async fn exhausting_retries_returns_upstream_error() {
        let (server, mut client) = fixture().await;

        // Always return 503 — all MAX_ATTEMPTS will fail.
        Mock::given(wm_method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = client.list_tools().await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("503"), "expected upstream 503 surfaced: {msg}");
    }

    #[tokio::test]
    async fn does_not_retry_on_400() {
        let (server, mut client) = fixture().await;

        // Single mock — if our code retried, wiremock would run out of matches
        // and we'd see a different error. Here we verify the single 400 is
        // surfaced directly.
        Mock::given(wm_method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad"))
            .expect(1)
            .mount(&server)
            .await;

        let err = client.list_tools().await.unwrap_err();
        assert!(format!("{err:#}").contains("400"));
        // .expect(1) above asserts we called exactly once (no retries).
    }

    #[tokio::test]
    async fn does_not_retry_on_500() {
        // 500 = application error, retrying won't help.
        let (server, mut client) = fixture().await;
        Mock::given(wm_method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;
        let err = client.list_tools().await.unwrap_err();
        assert!(format!("{err:#}").contains("500"));
    }

    // --- close() / DELETE ---------------------------------------------------

    #[tokio::test]
    async fn close_sends_delete_with_session_id() {
        let (server, mut client) = fixture().await;
        client.session_id = Some("sess-to-close".into());

        Mock::given(wm_method("DELETE"))
            .and(header("Mcp-Session-Id", "sess-to-close"))
            .and(header_exists("Authorization"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        client.close().await.unwrap();
        assert!(client.session_id.is_none(), "session_id should be cleared");
    }

    #[tokio::test]
    async fn close_tolerates_404_from_already_evicted_session() {
        let (server, mut client) = fixture().await;
        client.session_id = Some("stale".into());

        Mock::given(wm_method("DELETE"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        client.close().await.unwrap(); // 404 is OK
    }

    #[tokio::test]
    async fn close_is_noop_without_session_id() {
        let (_, mut client) = fixture().await;
        // Never set session_id — close() should return Ok without hitting
        // the network at all (mock server has no handler registered; if
        // we did make a request, it would hang waiting for a match).
        client.close().await.unwrap();
    }

    // --- is_retryable_status (pure helper) ---------------------------------

    #[test]
    fn retryable_status_matrix() {
        for code in [429u16, 502, 503, 504] {
            assert!(is_retryable_status(StatusCode::from_u16(code).unwrap()));
        }
        for code in [200u16, 301, 400, 401, 403, 404, 500, 501, 505] {
            assert!(!is_retryable_status(StatusCode::from_u16(code).unwrap()));
        }
    }
}
