pub mod protocol;
pub mod tools;

pub use protocol::*;
pub use tools::*;

use anyhow::Result;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

enum IncomingRpc {
    Request(JsonRpcRequest),
    ParseError(String),
    TooLarge,
    TooLargeFatal,
}

/// Hard cap on one JSON-RPC frame (line or Content-Length payload).
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;
const MAX_HEADERS_PER_FRAME: usize = 32;
const MAX_IN_FLIGHT_RPC: usize = 64;

enum LineRead {
    Eof,
    Line,
    Oversized,
}

enum FrameRead {
    Eof,
    Frame,
    Oversized,
    /// Oversized Content-Length desynchronizes framed stdio; fail immediately without hanging.
    OversizedFatal,
    InvalidHeader(String),
}

fn lock_unpoisoned<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads one `\n`-terminated line into `buf`, capping at `max_bytes`.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<LineRead> {
    buf.clear();
    let mut oversized = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(match (oversized, buf.is_empty()) {
                (true, _) => LineRead::Oversized,
                (false, true) => LineRead::Eof,
                (false, false) => LineRead::Line,
            });
        }
        let (take, found_nl) = match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !oversized {
            if buf.len() + take > max_bytes {
                oversized = true;
                buf.clear();
            } else {
                buf.extend_from_slice(&chunk[..take]);
            }
        }
        reader.consume(take);
        if found_nl {
            return Ok(if oversized {
                LineRead::Oversized
            } else {
                LineRead::Line
            });
        }
    }
}

fn parse_header_line(line: &str) -> Option<(&str, &str)> {
    let (name, val) = line.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    Some((name, val.trim()))
}

/// Reads either an HTTP/LSP-framed (`Content-Length:` in any header position)
/// or legacy newline-delimited JSON-RPC frame into `buf`.
fn read_rpc_frame<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<FrameRead> {
    loop {
        match read_bounded_line(reader, buf, MAX_LINE_BYTES)? {
            LineRead::Eof => return Ok(FrameRead::Eof),
            LineRead::Oversized => return Ok(FrameRead::Oversized),
            LineRead::Line => {}
        }
        let Ok(line_str) = std::str::from_utf8(buf) else {
            return Ok(FrameRead::InvalidHeader(
                "Invalid UTF-8 in JSON-RPC input".to_string(),
            ));
        };
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            continue;
        }
        // JSON payload directly on a line (NDJSON framing).
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return Ok(FrameRead::Frame);
        }

        let Some((first_name, first_val)) = parse_header_line(trimmed) else {
            return Ok(FrameRead::Frame);
        };

        let mut content_len: Option<usize> = None;
        if first_name.eq_ignore_ascii_case("content-length") {
            match first_val.parse::<usize>() {
                Ok(n) => content_len = Some(n),
                Err(_) => {
                    return Ok(FrameRead::InvalidHeader(format!(
                        "Invalid Content-Length header: {}",
                        trimmed
                    )));
                }
            }
        }

        let mut hdr_buf = Vec::new();
        let mut header_count = 1usize;
        loop {
            match read_bounded_line(reader, &mut hdr_buf, MAX_HEADER_LINE_BYTES)? {
                LineRead::Eof => return Ok(FrameRead::Eof),
                LineRead::Oversized => return Ok(FrameRead::OversizedFatal),
                LineRead::Line => {}
            }
            let Ok(h_str) = std::str::from_utf8(&hdr_buf) else {
                return Ok(FrameRead::InvalidHeader(
                    "Invalid UTF-8 in MCP header".to_string(),
                ));
            };
            let h_trim = h_str.trim();
            if h_trim.is_empty() {
                break;
            }
            header_count += 1;
            if header_count > MAX_HEADERS_PER_FRAME {
                return Ok(FrameRead::InvalidHeader(
                    "Too many headers in MCP frame".to_string(),
                ));
            }
            if let Some((name, val)) = parse_header_line(h_trim) {
                if name.eq_ignore_ascii_case("content-length") {
                    match val.parse::<usize>() {
                        Ok(n) => content_len = Some(n),
                        Err(_) => {
                            return Ok(FrameRead::InvalidHeader(format!(
                                "Invalid Content-Length header: {}",
                                h_trim
                            )));
                        }
                    }
                }
            }
        }

        let Some(len) = content_len else {
            return Ok(FrameRead::InvalidHeader(
                "Missing Content-Length header".to_string(),
            ));
        };
        if len > MAX_LINE_BYTES {
            return Ok(FrameRead::OversizedFatal);
        }

        buf.clear();
        buf.resize(len, 0);
        reader.read_exact(buf)?;
        if std::str::from_utf8(buf).is_err() {
            return Ok(FrameRead::InvalidHeader(
                "Invalid UTF-8 in JSON-RPC payload".to_string(),
            ));
        }
        return Ok(FrameRead::Frame);
    }
}

fn spawn_reader_thread(
    tx: mpsc::SyncSender<IncomingRpc>,
    in_flight: Arc<Mutex<HashMap<Value, bool>>>,
) {
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut raw = Vec::new();
        loop {
            match read_rpc_frame(&mut stdin, &mut raw) {
                Ok(FrameRead::Frame) => {}
                Ok(FrameRead::Oversized) => {
                    if tx.send(IncomingRpc::TooLarge).is_err() {
                        break;
                    }
                    continue;
                }
                Ok(FrameRead::OversizedFatal) => {
                    let _ = tx.send(IncomingRpc::TooLargeFatal);
                    break;
                }
                Ok(FrameRead::InvalidHeader(msg)) => {
                    if tx.send(IncomingRpc::ParseError(msg)).is_err() {
                        break;
                    }
                    continue;
                }
                Ok(FrameRead::Eof) | Err(_) => break,
            }

            let Ok(payload) = std::str::from_utf8(&raw) else {
                let _ = tx.send(IncomingRpc::ParseError("Invalid UTF-8 payload".into()));
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonRpcRequest>(payload) {
                Ok(req) => {
                    if handle_incoming_cancellation(&req, &in_flight) {
                        continue;
                    }
                    let tracked_id = trackable_id(&req);
                    if let Some(ref id) = tracked_id {
                        lock_unpoisoned(&in_flight).insert(id.clone(), false);
                    }
                    if tx.send(IncomingRpc::Request(req)).is_err() {
                        if let Some(ref id) = tracked_id {
                            lock_unpoisoned(&in_flight).remove(id);
                        }
                        break;
                    }
                }
                Err(e) => {
                    if tx
                        .send(IncomingRpc::ParseError(format!("Parse error: {}", e)))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
}

fn handle_incoming_cancellation(
    req: &JsonRpcRequest,
    in_flight: &Mutex<HashMap<Value, bool>>,
) -> bool {
    if req.method != "notifications/cancelled" {
        return false;
    }
    if let Some(req_id) = req.params.as_ref().and_then(|p| p.get("requestId")) {
        if let Some(cancelled) = lock_unpoisoned(in_flight).get_mut(req_id) {
            *cancelled = true;
        }
    }
    true
}

fn trackable_id(req: &JsonRpcRequest) -> Option<Value> {
    if req.method == "initialize" {
        return None;
    }
    req.id.clone().filter(|v| !v.is_null())
}

pub fn run_mcp_server(workspace: Option<PathBuf>) -> Result<()> {
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let default_root = workspace
        .map(|w| crate::workspace::walker::find_workspace_root(None, &w))
        .unwrap_or_else(|| crate::workspace::walker::find_workspace_root(None, &current_dir));

    let canonical_root = default_root.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "Failed to resolve workspace root '{}': {}",
            default_root.display(),
            e
        )
    })?;

    if crate::workspace::walker::is_disallowed_workspace_root(&canonical_root) {
        anyhow::bail!(
            "Refusing to run MCP server with home or system directory as workspace root: '{}'",
            canonical_root.display()
        );
    }

    eprintln!(
        "mimori MCP server running on stdio (root: {})",
        canonical_root.display()
    );

    let session = McpSession::new(canonical_root);
    let mut cache = McpCache::new();
    let mut stdout = io::stdout();

    let (tx, rx) = mpsc::sync_channel(MAX_IN_FLIGHT_RPC);
    let in_flight: Arc<Mutex<HashMap<Value, bool>>> = Arc::new(Mutex::new(HashMap::new()));
    spawn_reader_thread(tx, Arc::clone(&in_flight));

    for msg in rx {
        match msg {
            IncomingRpc::ParseError(err_msg) => {
                let resp = JsonRpcResponse::error(Value::Null, -32700, err_msg);
                send_response(&mut stdout, &resp)?;
            }
            IncomingRpc::TooLarge => {
                let resp = JsonRpcResponse::error(
                    Value::Null,
                    -32600,
                    format!("Request exceeds {} byte limit", MAX_LINE_BYTES),
                );
                send_response(&mut stdout, &resp)?;
            }
            IncomingRpc::TooLargeFatal => {
                let resp = JsonRpcResponse::error(
                    Value::Null,
                    -32600,
                    format!(
                        "Framed request exceeds {} byte limit; closing stream",
                        MAX_LINE_BYTES
                    ),
                );
                send_response(&mut stdout, &resp)?;
                break;
            }
            IncomingRpc::Request(req) => {
                let tracked_id = trackable_id(&req);
                if let Some(ref id) = tracked_id {
                    let mut map = lock_unpoisoned(&in_flight);
                    if map.get(id) == Some(&true) {
                        map.remove(id);
                        continue;
                    }
                }

                let maybe_resp = handle_request(req, &session, &mut cache);
                let was_cancelled = match tracked_id {
                    Some(ref id) => lock_unpoisoned(&in_flight).remove(id) == Some(true),
                    None => false,
                };
                if !was_cancelled {
                    if let Some(resp) = maybe_resp {
                        send_response(&mut stdout, &resp)?;
                    }
                }
            }
        }
    }

    Ok(())
}

pub fn handle_request(
    req: JsonRpcRequest,
    session: &McpSession,
    cache: &mut McpCache,
) -> Option<JsonRpcResponse> {
    let req_id = match req.id {
        Some(Value::Null) => {
            return Some(JsonRpcResponse::error(
                Value::Null,
                -32600,
                "Invalid Request: 'id' must not be null",
            ));
        }
        Some(id) => id,
        None => return None,
    };

    if req.jsonrpc != "2.0" {
        return Some(JsonRpcResponse::error(
            req_id,
            -32600,
            format!(
                "Invalid Request: expected jsonrpc '2.0', got '{}'",
                req.jsonrpc
            ),
        ));
    }

    match req.method.as_str() {
        "initialize" => Some(JsonRpcResponse::success(
            req_id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "mimori",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "mimori AST navigation & in-repo agent memory: orient via mimori_dump or mimori_map; search with mimori_find or mimori_missing; inspect symbols with mimori_slice (auto-injects scoped memory); trace impact with mimori_graph, mimori_blast, mimori_diff, and mimori_doctor; manage memory/debt via mimori_memory and mimori_debt."
            }),
        )),
        "ping" => Some(JsonRpcResponse::success(req_id, json!({}))),
        "tools/list" => Some(JsonRpcResponse::success(
            req_id,
            json!({ "tools": tools::list_tools() }),
        )),
        "tools/call" => Some(dispatch_tool_call(req_id, req.params, session, cache)),
        _ => Some(JsonRpcResponse::error(
            req_id,
            -32601,
            format!("Method not found: {}", req.method),
        )),
    }
}

fn dispatch_tool_call(
    req_id: Value,
    params: Option<Value>,
    session: &McpSession,
    cache: &mut McpCache,
) -> JsonRpcResponse {
    let Some(ref p) = params.filter(Value::is_object) else {
        return JsonRpcResponse::error(req_id, -32602, "Invalid params: expected an object");
    };
    let Some(tool_name) = p
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
    else {
        return JsonRpcResponse::error(
            req_id,
            -32602,
            "Invalid params: 'name' is required and must be a non-empty string",
        );
    };
    let empty_args = Value::Object(Map::new());
    let arguments = match p.get("arguments") {
        None => &empty_args,
        Some(args) if args.is_object() => args,
        Some(_) => {
            return JsonRpcResponse::error(
                req_id,
                -32602,
                "Invalid params: 'arguments' must be an object",
            );
        }
    };

    match tools::call_tool(tool_name, arguments, session, cache) {
        Ok(outcome) => JsonRpcResponse::success(
            req_id,
            json!({
                "content": [{ "type": "text", "text": outcome.text }],
                "isError": outcome.is_error
            }),
        ),
        Err(ToolError::InvalidParams(err_msg)) => JsonRpcResponse::error(req_id, -32602, err_msg),
        Err(ToolError::NotFound(name)) => JsonRpcResponse::success(
            req_id,
            json!({
                "content": [{ "type": "text", "text": format!("Error: Unknown tool: '{}'", name) }],
                "isError": true
            }),
        ),
        Err(ToolError::Execution(err_msg)) => JsonRpcResponse::success(
            req_id,
            json!({
                "content": [{ "type": "text", "text": format!("Error: {}", err_msg) }],
                "isError": true
            }),
        ),
    }
}

fn send_response(stdout: &mut io::Stdout, resp: &JsonRpcResponse) -> Result<()> {
    let json_str = serde_json::to_string(resp)?;
    stdout.write_all(json_str.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn framed_reader_accepts_content_type_before_content_length() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let wire = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let mut cursor = Cursor::new(wire.as_bytes());
        let mut buf = Vec::new();
        assert!(matches!(
            read_rpc_frame(&mut cursor, &mut buf).unwrap(),
            FrameRead::Frame
        ));
        assert_eq!(std::str::from_utf8(&buf).unwrap(), body);
    }

    #[test]
    fn oversized_content_length_fails_immediately_without_blocking() {
        let wire = b"Content-Length: 999999999\r\n\r\n";
        let mut cursor = Cursor::new(wire.as_slice());
        let mut buf = Vec::new();
        assert!(matches!(
            read_rpc_frame(&mut cursor, &mut buf).unwrap(),
            FrameRead::OversizedFatal
        ));
    }
}
