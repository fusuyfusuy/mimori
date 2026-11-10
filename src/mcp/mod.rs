pub mod protocol;
pub mod tools;

pub use protocol::*;
pub use tools::*;

use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

enum IncomingRpc {
    Request(JsonRpcRequest),
    ParseError(String),
    TooLarge,
}

/// Hard cap on one JSON-RPC frame (line or Content-Length payload).
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
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
    InvalidHeader(String),
}

/// Reads one `\n`-terminated line into `buf`, never holding more than
/// `MAX_LINE_BYTES`. An oversized line is drained to its newline and discarded.
fn read_bounded_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<LineRead> {
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
            if buf.len() + take > MAX_LINE_BYTES {
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

/// Reads either a `Content-Length: <N>\r\n\r\n<payload>` frame or a legacy
/// newline-delimited JSON-RPC frame into `buf`, enforcing `MAX_LINE_BYTES`.
fn read_rpc_frame<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<FrameRead> {
    loop {
        match read_bounded_line(reader, buf)? {
            LineRead::Eof => return Ok(FrameRead::Eof),
            LineRead::Oversized => return Ok(FrameRead::Oversized),
            LineRead::Line => {}
        }
        let line_str = String::from_utf8_lossy(buf);
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((hdr_name, hdr_val)) = trimmed.split_once(':') else {
            return Ok(FrameRead::Frame);
        };
        if !hdr_name.trim().eq_ignore_ascii_case("content-length") {
            return Ok(FrameRead::Frame);
        }
        let Ok(content_len) = hdr_val.trim().parse::<usize>() else {
            return Ok(FrameRead::InvalidHeader(format!(
                "Invalid Content-Length header: {}",
                trimmed
            )));
        };

        // Drain any remaining HTTP-style headers up to the blank `\r\n` separator line.
        let mut hdr_buf = Vec::new();
        loop {
            match read_bounded_line(reader, &mut hdr_buf)? {
                LineRead::Eof => return Ok(FrameRead::Eof),
                LineRead::Oversized => return Ok(FrameRead::Oversized),
                LineRead::Line => {
                    if String::from_utf8_lossy(&hdr_buf).trim().is_empty() {
                        break;
                    }
                }
            }
        }

        if content_len > MAX_LINE_BYTES {
            let _ = io::copy(&mut reader.take(content_len as u64), &mut io::sink())?;
            return Ok(FrameRead::Oversized);
        }

        buf.clear();
        buf.resize(content_len, 0);
        reader.read_exact(buf)?;
        return Ok(FrameRead::Frame);
    }
}

pub fn run_mcp_server(workspace: Option<PathBuf>) -> Result<()> {
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let default_root = workspace
        .map(|w| crate::workspace::walker::find_workspace_root(None, &w))
        .unwrap_or_else(|| crate::workspace::walker::find_workspace_root(None, &current_dir));

    let canonical_root = match default_root.canonicalize() {
        Ok(r) => r,
        Err(e) => {
            anyhow::bail!(
                "Failed to resolve workspace root '{}': {}",
                default_root.display(),
                e
            );
        }
    };

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
    // Maps currently queued/executing cancellable request IDs to `cancelled: bool`.
    // Bounded by `MAX_IN_FLIGHT_RPC + 1`: late cancellations for unknown/finished
    // IDs are ignored and never poison future reused request IDs.
    let in_flight: Arc<Mutex<HashMap<serde_json::Value, bool>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let in_flight_reader = Arc::clone(&in_flight);

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
                Ok(FrameRead::InvalidHeader(msg)) => {
                    if tx.send(IncomingRpc::ParseError(msg)).is_err() {
                        break;
                    }
                    continue;
                }
                Ok(FrameRead::Eof) | Err(_) => break,
            }
            let payload = String::from_utf8_lossy(&raw);
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonRpcRequest>(payload) {
                Ok(req) => {
                    if req.method == "notifications/cancelled" {
                        if let Some(params) = &req.params {
                            if let Some(req_id) = params.get("requestId") {
                                if let Some(cancelled) =
                                    in_flight_reader.lock().unwrap().get_mut(req_id)
                                {
                                    *cancelled = true;
                                }
                            }
                        }
                        continue;
                    }
                    // Per MCP lifecycle spec, `initialize` MUST NOT be cancelled.
                    let tracked_id = if req.method != "initialize" {
                        req.id.clone()
                    } else {
                        None
                    };
                    if let Some(ref id) = tracked_id {
                        in_flight_reader.lock().unwrap().insert(id.clone(), false);
                    }
                    if tx.send(IncomingRpc::Request(req)).is_err() {
                        if let Some(ref id) = tracked_id {
                            in_flight_reader.lock().unwrap().remove(id);
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

    for msg in rx {
        match msg {
            IncomingRpc::ParseError(err_msg) => {
                let resp = JsonRpcResponse::error(serde_json::Value::Null, -32700, err_msg);
                send_response(&mut stdout, &resp)?;
            }
            IncomingRpc::TooLarge => {
                let resp = JsonRpcResponse::error(
                    serde_json::Value::Null,
                    -32600,
                    format!("Request exceeds {} byte limit", MAX_LINE_BYTES),
                );
                send_response(&mut stdout, &resp)?;
            }
            IncomingRpc::Request(req) => {
                let tracked_id = if req.method != "initialize" {
                    req.id.clone()
                } else {
                    None
                };
                if let Some(ref id) = tracked_id {
                    let mut map = in_flight.lock().unwrap();
                    if map.get(id) == Some(&true) {
                        map.remove(id);
                        continue;
                    }
                }

                let maybe_resp = handle_request(req, &session, &mut cache);
                let was_cancelled = match tracked_id {
                    Some(ref id) => in_flight.lock().unwrap().remove(id) == Some(true),
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
        Some(id) => id,
        None => {
            // Notification: per JSON-RPC 2.0, server MUST NOT reply
            return None;
        }
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
        "initialize" => {
            // mimori deliberately declares and supports protocolVersion 2024-11-05 (JSON-RPC 2.0 over stdio).
            // Modern MCP clients advertising newer capabilities gracefully fall back to legacy line framing
            // when answered with 2024-11-05.
            let resp_data = json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "mimori",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": "mimori AST code intelligence engine: use mimori_map for codebase topology, mimori_find for symbol search, mimori_slice for token-dense symbol context with callers/callees, mimori_graph for relationship traversal, and mimori_blast for impact radius."
            });
            Some(JsonRpcResponse::success(req_id, resp_data))
        }
        "ping" => Some(JsonRpcResponse::success(req_id, json!({}))),
        "tools/list" => {
            let tools = tools::list_tools();
            Some(JsonRpcResponse::success(req_id, json!({ "tools": tools })))
        }
        "tools/call" => {
            let params = match req.params {
                Some(ref p) if p.is_object() => p,
                _ => {
                    return Some(JsonRpcResponse::error(
                        req_id,
                        -32602,
                        "Invalid params: expected an object",
                    ));
                }
            };
            let tool_name = match params.get("name").and_then(|v| v.as_str()) {
                Some(name) if !name.is_empty() => name,
                _ => {
                    return Some(JsonRpcResponse::error(
                        req_id,
                        -32602,
                        "Invalid params: 'name' is required and must be a non-empty string",
                    ));
                }
            };
            let arguments = match params.get("arguments") {
                Some(args) if args.is_object() => args,
                _ => {
                    return Some(JsonRpcResponse::error(
                        req_id,
                        -32602,
                        "Invalid params: 'arguments' is required and must be an object",
                    ));
                }
            };

            match tools::call_tool(tool_name, arguments, session, cache) {
                Ok(outcome) => Some(JsonRpcResponse::success(
                    req_id,
                    json!({
                        "content": [
                            {
                                "type": "text",
                                "text": outcome.text
                            }
                        ],
                        "isError": outcome.is_error
                    }),
                )),
                Err(ToolError::InvalidParams(err_msg)) => {
                    Some(JsonRpcResponse::error(req_id, -32602, err_msg))
                }
                Err(ToolError::NotFound(name)) => Some(JsonRpcResponse::success(
                    req_id,
                    json!({
                        "content": [
                            {
                                "type": "text",
                                "text": format!("Error: Unknown tool: '{}'", name)
                            }
                        ],
                        "isError": true
                    }),
                )),
                Err(ToolError::Execution(err_msg)) => Some(JsonRpcResponse::success(
                    req_id,
                    json!({
                        "content": [
                            {
                                "type": "text",
                                "text": format!("Error: {}", err_msg)
                            }
                        ],
                        "isError": true
                    }),
                )),
            }
        }
        _ => Some(JsonRpcResponse::error(
            req_id,
            -32601,
            format!("Method not found: {}", req.method),
        )),
    }
}

fn send_response(stdout: &mut io::Stdout, resp: &JsonRpcResponse) -> Result<()> {
    let json_str = serde_json::to_string(resp)?;
    stdout.write_all(json_str.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
