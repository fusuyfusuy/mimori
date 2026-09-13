pub mod protocol;
pub mod tools;

pub use protocol::*;
pub use tools::*;

use anyhow::Result;
use serde_json::json;
use std::collections::HashSet;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

enum IncomingRpc {
    Request(JsonRpcRequest),
    ParseError(String),
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

    eprintln!(
        "mimori MCP server running on stdio (root: {})",
        canonical_root.display()
    );

    let session = McpSession::new(canonical_root);
    let mut cache = McpCache::new();
    let mut stdout = io::stdout();

    let (tx, rx) = mpsc::channel();
    let cancelled_ids: Arc<Mutex<HashSet<serde_json::Value>>> =
        Arc::new(Mutex::new(HashSet::new()));
    let cancelled_ids_reader = Arc::clone(&cancelled_ids);

    std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<JsonRpcRequest>(line) {
                Ok(req) => {
                    if req.method == "notifications/cancelled" {
                        if let Some(params) = &req.params {
                            if let Some(req_id) = params.get("requestId") {
                                cancelled_ids_reader.lock().unwrap().insert(req_id.clone());
                            }
                        }
                        continue;
                    }
                    if tx.send(IncomingRpc::Request(req)).is_err() {
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
            IncomingRpc::Request(req) => {
                let req_id = req.id.clone();
                if let Some(ref id) = req_id {
                    if cancelled_ids.lock().unwrap().remove(id) {
                        continue;
                    }
                }

                if let Some(resp) = handle_request(req, &session, &mut cache) {
                    if let Some(ref id) = req_id {
                        if cancelled_ids.lock().unwrap().remove(id) {
                            continue;
                        }
                    }
                    send_response(&mut stdout, &resp)?;
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
                Ok(text) => Some(JsonRpcResponse::success(
                    req_id,
                    json!({
                        "content": [
                            {
                                "type": "text",
                                "text": text
                            }
                        ],
                        "isError": false
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
