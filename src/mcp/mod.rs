pub mod protocol;
pub mod tools;

pub use protocol::*;
pub use tools::*;

use anyhow::Result;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};

enum IncomingRpc {
    Request {
        seq: Option<u64>,
        req: JsonRpcRequest,
    },
    ImmediateError(JsonRpcResponse),
    TooLarge,
    TooLargeFatal,
}

/// Hard cap on one JSON-RPC frame (line or Content-Length payload).
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_LINE_BYTES: usize = 8 * 1024;
const MAX_HEADERS_PER_FRAME: usize = 32;
const MAX_IN_FLIGHT_RPC: usize = 64;

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

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

/// Tracks queued/active requests by monotonic sequence ID so duplicate request
/// IDs never overwrite each other or drop cancellation state when one finishes.
#[derive(Default)]
struct InFlightTracker {
    next_seq: u64,
    entries: HashMap<u64, (Value, bool)>,
}

impl InFlightTracker {
    fn register(&mut self, req_id: Value) -> u64 {
        self.next_seq = self.next_seq.wrapping_add(1);
        let seq = self.next_seq;
        self.entries.insert(seq, (req_id, false));
        seq
    }

    fn cancel(&mut self, req_id: &Value) {
        for (id, cancelled) in self.entries.values_mut() {
            if id == req_id {
                *cancelled = true;
            }
        }
    }

    fn take_if_cancelled(&mut self, seq: u64) -> bool {
        if self.entries.get(&seq).is_some_and(|(_, c)| *c) {
            self.entries.remove(&seq);
            return true;
        }
        false
    }

    fn finish(&mut self, seq: u64) -> bool {
        self.entries.remove(&seq).is_some_and(|(_, c)| c)
    }

    fn remove(&mut self, seq: u64) {
        self.entries.remove(&seq);
    }
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

fn is_recognized_mcp_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("content-type")
}

fn apply_header_field(
    name: &str,
    val: &str,
    raw_line: &str,
    content_len: &mut Option<usize>,
    first_err: &mut Option<String>,
) {
    if !name.eq_ignore_ascii_case("content-length") {
        return;
    }
    if content_len.is_some() {
        first_err.get_or_insert_with(|| "Duplicate Content-Length header".to_string());
        return;
    }
    match val.parse::<usize>() {
        Ok(n) => *content_len = Some(n),
        Err(_) => {
            first_err.get_or_insert_with(|| format!("Invalid Content-Length header: {}", raw_line));
        }
    }
}

fn drain_header_block<R: BufRead>(
    reader: &mut R,
    content_len: &mut Option<usize>,
    first_err: &mut Option<String>,
) -> io::Result<Option<FrameRead>> {
    let mut hdr_buf = Vec::new();
    let mut header_count = 1usize;
    loop {
        match read_bounded_line(reader, &mut hdr_buf, MAX_HEADER_LINE_BYTES)? {
            LineRead::Eof => return Ok(Some(FrameRead::Eof)),
            LineRead::Oversized => return Ok(Some(FrameRead::OversizedFatal)),
            LineRead::Line => {}
        }
        let Ok(h_str) = std::str::from_utf8(&hdr_buf) else {
            first_err.get_or_insert_with(|| "Invalid UTF-8 in MCP header".to_string());
            continue;
        };
        let h_trim = h_str.trim();
        if h_trim.is_empty() {
            return Ok(None);
        }
        header_count += 1;
        if header_count > MAX_HEADERS_PER_FRAME {
            first_err.get_or_insert_with(|| "Too many headers in MCP frame".to_string());
            continue;
        }
        if let Some((name, val)) = parse_header_line(h_trim) {
            apply_header_field(name, val, h_trim, content_len, first_err);
        }
    }
}

fn read_framed_body<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    first_name: &str,
    first_val: &str,
    first_line: &str,
) -> io::Result<FrameRead> {
    let mut content_len: Option<usize> = None;
    let mut first_err: Option<String> = None;
    apply_header_field(
        first_name,
        first_val,
        first_line,
        &mut content_len,
        &mut first_err,
    );

    if let Some(early) = drain_header_block(reader, &mut content_len, &mut first_err)? {
        return Ok(early);
    }
    if let Some(err) = first_err {
        return Ok(FrameRead::InvalidHeader(err));
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
    Ok(FrameRead::Frame)
}

/// Reads either an HTTP/LSP-framed (`Content-Length:` / `Content-Type:`)
/// or newline-delimited JSON-RPC frame into `buf`.
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
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return Ok(FrameRead::Frame);
        }

        // Only enter header framing mode on recognized MCP headers so arbitrary
        // non-JSON text containing ':' fails once as a JSON parse error instead
        // of swallowing subsequent NDJSON frames.
        let Some((first_name, first_val)) = parse_header_line(trimmed) else {
            return Ok(FrameRead::Frame);
        };
        if !is_recognized_mcp_header(first_name) {
            return Ok(FrameRead::Frame);
        }

        let name_owned = first_name.to_string();
        let val_owned = first_val.to_string();
        let line_owned = trimmed.to_string();
        return read_framed_body(reader, buf, &name_owned, &val_owned, &line_owned);
    }
}

fn dispatch_raw_payload(
    payload: &str,
    tx: &mpsc::SyncSender<IncomingRpc>,
    in_flight: &Mutex<InFlightTracker>,
) -> bool {
    match JsonRpcRequest::parse_json(payload) {
        Ok(req) => {
            if handle_incoming_cancellation(&req, in_flight) {
                return true;
            }
            let seq = trackable_id(&req).map(|id| lock_unpoisoned(in_flight).register(id));
            if tx.send(IncomingRpc::Request { seq, req }).is_err() {
                if let Some(s) = seq {
                    lock_unpoisoned(in_flight).remove(s);
                }
                return false;
            }
            true
        }
        Err(err_resp) => tx.send(IncomingRpc::ImmediateError(*err_resp)).is_ok(),
    }
}

fn spawn_reader_thread(tx: mpsc::SyncSender<IncomingRpc>, in_flight: Arc<Mutex<InFlightTracker>>) {
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
                    let resp = JsonRpcResponse::error(Value::Null, -32700, msg);
                    if tx.send(IncomingRpc::ImmediateError(resp)).is_err() {
                        break;
                    }
                    continue;
                }
                Ok(FrameRead::Eof) | Err(_) => break,
            }

            let Ok(payload) = std::str::from_utf8(&raw) else {
                let resp = JsonRpcResponse::error(Value::Null, -32700, "Invalid UTF-8 payload");
                if tx.send(IncomingRpc::ImmediateError(resp)).is_err() {
                    break;
                }
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }
            if !dispatch_raw_payload(payload, &tx, &in_flight) {
                break;
            }
        }
    });
}

fn handle_incoming_cancellation(req: &JsonRpcRequest, in_flight: &Mutex<InFlightTracker>) -> bool {
    if req.method != "notifications/cancelled" {
        return false;
    }
    if let Some(req_id) = req.params.as_ref().and_then(|p| p.get("requestId")) {
        lock_unpoisoned(in_flight).cancel(req_id);
    }
    true
}

fn trackable_id(req: &JsonRpcRequest) -> Option<Value> {
    if req.method == "initialize" {
        return None;
    }
    req.id.clone().filter(is_valid_request_id)
}

/// Resolve MCP server root strictly: an explicit `--workspace` locks directly
/// to that canonical existing directory (no ancestor `.git` walk-up).
fn resolve_mcp_root(workspace: Option<PathBuf>, current_dir: &Path) -> Result<PathBuf> {
    let canonical_root = match workspace {
        Some(w) => {
            let candidate = if w.is_absolute() {
                w.clone()
            } else {
                current_dir.join(&w)
            };
            let canon = candidate
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("Invalid --workspace '{}': {}", w.display(), e))?;
            if !canon.is_dir() {
                anyhow::bail!(
                    "Invalid --workspace '{}': path is not a directory",
                    w.display()
                );
            }
            canon
        }
        None => {
            let found = crate::workspace::walker::find_workspace_root(None, current_dir);
            let canon = found.canonicalize().map_err(|e| {
                anyhow::anyhow!(
                    "Failed to resolve workspace root '{}': {}",
                    found.display(),
                    e
                )
            })?;
            if !canon.is_dir() {
                anyhow::bail!("Workspace root '{}' is not a directory", canon.display());
            }
            canon
        }
    };

    if crate::workspace::walker::is_disallowed_workspace_root(&canonical_root) {
        anyhow::bail!(
            "Refusing to run MCP server with home or system directory as workspace root: '{}'",
            canonical_root.display()
        );
    }

    Ok(canonical_root)
}

pub fn run_mcp_server(workspace: Option<PathBuf>) -> Result<()> {
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let canonical_root = resolve_mcp_root(workspace, &current_dir)?;

    eprintln!(
        "mimori MCP server running on stdio (root: {})",
        canonical_root.display()
    );

    let session = McpSession::new(canonical_root);
    let mut cache = McpCache::new();
    let mut stdout = io::stdout();

    let (tx, rx) = mpsc::sync_channel(MAX_IN_FLIGHT_RPC);
    let in_flight: Arc<Mutex<InFlightTracker>> = Arc::new(Mutex::new(InFlightTracker::default()));
    spawn_reader_thread(tx, Arc::clone(&in_flight));

    for msg in rx {
        match msg {
            IncomingRpc::ImmediateError(resp) => {
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
            IncomingRpc::Request { seq, req } => {
                if let Some(s) = seq {
                    if lock_unpoisoned(&in_flight).take_if_cancelled(s) {
                        continue;
                    }
                }

                let maybe_resp = handle_request(req, &session, &mut cache);
                let was_cancelled = seq.is_some_and(|s| lock_unpoisoned(&in_flight).finish(s));
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
        Some(id) if is_valid_request_id(&id) => id,
        Some(_) => {
            return Some(JsonRpcResponse::error(
                Value::Null,
                -32600,
                "Invalid Request: 'id' must be a string or number",
            ));
        }
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
        "initialize" => Some(handle_initialize(req_id, req.params)),
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

fn handle_initialize(req_id: Value, params: Option<Value>) -> JsonRpcResponse {
    let requested_version = match params {
        None | Some(Value::Null) => None,
        Some(Value::Object(ref map)) => match map.get("protocolVersion") {
            None | Some(Value::Null) => None,
            Some(Value::String(v)) => Some(v.as_str()),
            Some(_) => {
                return JsonRpcResponse::error(
                    req_id,
                    -32602,
                    "Invalid params: 'protocolVersion' must be a string",
                );
            }
        },
        Some(_) => {
            return JsonRpcResponse::error(req_id, -32602, "Invalid params: expected an object");
        }
    };

    let negotiated = requested_version
        .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);

    JsonRpcResponse::success(
        req_id,
        json!({
            "protocolVersion": negotiated,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "mimori",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "mimori AST navigation & in-repo agent memory: orient via mimori_dump or mimori_map; search with mimori_find or mimori_missing; inspect symbols with mimori_slice (auto-injects scoped memory); trace impact with mimori_graph, mimori_blast, mimori_diff, and mimori_doctor; manage memory/debt via mimori_memory and mimori_debt."
        }),
    )
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

    #[test]
    fn colon_line_does_not_swallow_next_ndjson_frame() {
        let wire = b"hello: world\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n";
        let mut cursor = Cursor::new(wire.as_slice());
        let mut buf = Vec::new();

        assert!(matches!(
            read_rpc_frame(&mut cursor, &mut buf).unwrap(),
            FrameRead::Frame
        ));
        let first = std::str::from_utf8(&buf).unwrap().trim();
        let err = JsonRpcRequest::parse_json(first).unwrap_err();
        assert_eq!(err.error.as_ref().unwrap().code, -32700);

        assert!(matches!(
            read_rpc_frame(&mut cursor, &mut buf).unwrap(),
            FrameRead::Frame
        ));
        let second = std::str::from_utf8(&buf).unwrap().trim();
        let req = JsonRpcRequest::parse_json(second).unwrap();
        assert_eq!(req.id, Some(json!(2)));
        assert_eq!(req.method, "ping");
    }

    #[test]
    fn malformed_content_length_drains_headers_without_cascade() {
        let wire = b"Content-Length: -1\r\nContent-Type: a/b\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n";
        let mut cursor = Cursor::new(wire.as_slice());
        let mut buf = Vec::new();

        assert!(matches!(
            read_rpc_frame(&mut cursor, &mut buf).unwrap(),
            FrameRead::InvalidHeader(_)
        ));
        // Next frame must be the ping (not a second error from Content-Type).
        assert!(matches!(
            read_rpc_frame(&mut cursor, &mut buf).unwrap(),
            FrameRead::Frame
        ));
        let req = JsonRpcRequest::parse_json(std::str::from_utf8(&buf).unwrap().trim()).unwrap();
        assert_eq!(req.id, Some(json!(3)));
    }

    #[test]
    fn duplicate_content_length_rejected() {
        let wire = b"Content-Length: 5\r\nContent-Length: 39\r\n\r\n";
        let mut cursor = Cursor::new(wire.as_slice());
        let mut buf = Vec::new();
        match read_rpc_frame(&mut cursor, &mut buf).unwrap() {
            FrameRead::InvalidHeader(msg) => assert!(msg.contains("Duplicate"), "got: {}", msg),
            _ => panic!("expected InvalidHeader for duplicate Content-Length"),
        }
    }

    #[test]
    fn duplicate_id_cancellation() {
        let mut tracker = InFlightTracker::default();
        let seq1 = tracker.register(json!(7));
        tracker.cancel(&json!(7));
        let seq2 = tracker.register(json!(7));

        // First request was cancelled; second request with same id remains active.
        assert!(tracker.take_if_cancelled(seq1));
        assert!(!tracker.take_if_cancelled(seq2));
        // Completing seq1 must not drop tracking for seq2, so cancelling now hits seq2.
        tracker.cancel(&json!(7));
        assert!(tracker.finish(seq2));
    }

    #[test]
    fn parse_json_invalid_request_preserves_id_and_rejects_bad_id() {
        let missing_method =
            JsonRpcRequest::parse_json(r#"{"jsonrpc":"2.0","id":42}"#).unwrap_err();
        assert_eq!(missing_method.id, json!(42));
        assert_eq!(missing_method.error.as_ref().unwrap().code, -32600);

        let obj_id =
            JsonRpcRequest::parse_json(r#"{"jsonrpc":"2.0","id":{"bad":1},"method":"ping"}"#)
                .unwrap_err();
        assert_eq!(obj_id.id, Value::Null);
        assert_eq!(obj_id.error.as_ref().unwrap().code, -32600);

        let bool_id = JsonRpcRequest::parse_json(r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#)
            .unwrap_err();
        assert_eq!(bool_id.id, Value::Null);
        assert_eq!(bool_id.error.as_ref().unwrap().code, -32600);
    }

    #[test]
    fn initialize_validates_params_and_negotiates_protocol_version() {
        let bad = handle_initialize(json!(1), Some(json!("bad")));
        assert_eq!(bad.error.as_ref().unwrap().code, -32602);

        let v2025 = handle_initialize(json!(2), Some(json!({"protocolVersion": "2025-03-26"})));
        assert_eq!(v2025.result.unwrap()["protocolVersion"], "2025-03-26");

        let fallback = handle_initialize(json!(3), Some(json!({"protocolVersion": "2099-01-01"})));
        assert_eq!(fallback.result.unwrap()["protocolVersion"], "2024-11-05");
    }
}
