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

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::{json, Value};

pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const CLIENT_NAME: &str = "inderes-cli";
const SESSION_HEADER: &str = "Mcp-Session-Id";
const PROTOCOL_HEADER: &str = "MCP-Protocol-Version";

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

    async fn post(&self, body: &Value) -> Result<reqwest::Response> {
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
}
