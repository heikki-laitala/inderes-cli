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
    buf.find("\n\n").or_else(|| buf.find("\r\n\r\n")).map(|i| {
        i + if buf.as_bytes().get(i + 1) == Some(&b'\n') {
            2
        } else {
            4
        }
    })
}

fn parse_sse_event(event: &str) -> Result<Option<Value>> {
    let mut data_lines: Vec<&str> = Vec::new();
    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
        // `event:`, `id:`, `retry:` ignored — we only care about data.
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let joined = data_lines.join("\n");
    let v: Value = serde_json::from_str(&joined)
        .with_context(|| format!("parsing SSE data as JSON: {joined}"))?;
    Ok(Some(v))
}
