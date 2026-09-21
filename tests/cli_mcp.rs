use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use tempfile::tempdir;

struct McpClient {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl McpClient {
    fn spawn(cwd: &Path) -> Self {
        let mut child = Command::new(assert_cmd::cargo::cargo_bin("mimori"))
            .arg("mcp")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn mimori mcp");
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);
        Self { child, reader }
    }

    fn send(&mut self, val: &serde_json::Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        let s = serde_json::to_string(val).unwrap();
        stdin.write_all(s.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .expect("failed to read line from mcp stdout");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("invalid json: {}: line was: {}", e, line))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn test_mcp_handshake_and_ping() {
    let dir = tempdir().unwrap();
    let mut client = McpClient::spawn(dir.path());

    // 1. Initialize
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05"
        }
    }));

    let resp = client.recv();
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(resp["result"]["serverInfo"]["name"], "mimori");
    assert!(resp["result"]["capabilities"]["tools"].is_object());

    // 2. Notification (no response expected)
    client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    // 3. Ping
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "ping"
    }));

    let ping_resp = client.recv();
    assert_eq!(ping_resp["id"], 2);
    assert_eq!(ping_resp["result"], json!({}));
}

#[test]
fn test_mcp_tools_list() {
    let dir = tempdir().unwrap();
    let mut client = McpClient::spawn(dir.path());

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list"
    }));

    let resp = client.recv();
    assert_eq!(resp["id"], 1);
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "mimori_slice",
            "mimori_dump",
            "mimori_map",
            "mimori_find",
            "mimori_blast",
            "mimori_graph",
            "mimori_memory",
            "mimori_debt",
        ]
    );

    for tool in tools {
        assert!(tool["description"].is_string());
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool["inputSchema"]["properties"].is_object());
    }
}

#[test]
fn test_mcp_tools_call_workflow_and_caching() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    let db_file = src.join("db.rs");
    fs::write(
        &db_file,
        r#"
pub fn query_user(id: &str) -> String {
    format!("user_{}", id)
}
"#,
    )
    .unwrap();

    let service_file = src.join("service.rs");
    fs::write(
        &service_file,
        r#"
use crate::db::query_user;

pub fn get_profile(user_id: &str) -> String {
    query_user(user_id)
}
"#,
    )
    .unwrap();

    let mut client = McpClient::spawn(dir.path());

    // 1. Tool call: mimori_map
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": {
                "limit": 10
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 10);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Repository Map"), "got: {}", text);
    assert!(text.contains("query_user"), "got: {}", text);

    // 2. Tool call: mimori_find
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "tools/call",
        "params": {
            "name": "mimori_find",
            "arguments": {
                "pattern": "query_user"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 11);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("query_user"), "got: {}", text);

    // 3. Tool call: mimori_slice
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 12,
        "method": "tools/call",
        "params": {
            "name": "mimori_slice",
            "arguments": {
                "coordinate": "src/db.rs:query_user"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 12);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pub fn query_user"), "got: {}", text);
    assert!(text.contains("get_profile"), "got: {}", text); // 1-hop caller

    // 4. Tool call: mimori_graph (up)
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 13,
        "method": "tools/call",
        "params": {
            "name": "mimori_graph",
            "arguments": {
                "target": "src/db.rs:query_user",
                "direction": "up"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 13);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Upstream Callers"), "got: {}", text);
    assert!(text.contains("get_profile"), "got: {}", text);

    // 5. Tool call: mimori_graph (down)
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 14,
        "method": "tools/call",
        "params": {
            "name": "mimori_graph",
            "arguments": {
                "target": "src/service.rs:get_profile",
                "direction": "down"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 14);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Downstream Callees"), "got: {}", text);
    assert!(text.contains("query_user"), "got: {}", text);

    // 6. Tool call: mimori_blast
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 15,
        "method": "tools/call",
        "params": {
            "name": "mimori_blast",
            "arguments": {
                "target": "src/db.rs:query_user",
                "depth": 2
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 15);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("get_profile"), "got: {}", text);

    // 7. Test in-memory incremental cache update on file edit
    fs::write(
        &db_file,
        r#"
pub fn query_user(id: &str) -> String {
    format!("v2_user_{}", id)
}
"#,
    )
    .unwrap();

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 16,
        "method": "tools/call",
        "params": {
            "name": "mimori_slice",
            "arguments": {
                "coordinate": "src/db.rs:query_user"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 16);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("v2_user_"),
        "expected updated body, got: {}",
        text
    );

    // 8. Tool call: mimori_slice with numbered: true
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 17,
        "method": "tools/call",
        "params": {
            "name": "mimori_slice",
            "arguments": {
                "coordinate": "src/db.rs:query_user",
                "numbered": true
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 17);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("L2:"),
        "expected numbered lines, got: {}",
        text
    );

    // 9. Tool call: mimori_dump
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 18,
        "method": "tools/call",
        "params": {
            "name": "mimori_dump",
            "arguments": {
                "budget": 1500
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 18);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("MIMORI TURN-0 CONTEXT"),
        "expected turn-0 context, got: {}",
        text
    );
}

#[test]
fn test_mcp_error_handling() {
    let dir = tempdir().unwrap();
    let mut client = McpClient::spawn(dir.path());

    // 1. Unknown tool -> isError: true naming the tool
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/call",
        "params": {
            "name": "non_existent_tool",
            "arguments": {}
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 20);
    assert_eq!(resp["result"]["isError"], true);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("Unknown tool: 'non_existent_tool'"),
        "got: {}",
        text
    );

    // 2. Invalid direction for mimori_graph -> -32602 InvalidParams
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 21,
        "method": "tools/call",
        "params": {
            "name": "mimori_graph",
            "arguments": {
                "target": "foo:bar",
                "direction": "sideways"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 21);
    assert!(resp["error"].is_object());
    assert_eq!(resp["error"]["code"], -32602);
    assert!(resp["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Invalid direction"));

    // 3. Absent name -> -32602
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 22,
        "method": "tools/call",
        "params": {
            "arguments": {}
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 22);
    assert_eq!(resp["error"]["code"], -32602);

    // 4. Absent params -> -32602
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 23,
        "method": "tools/call"
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 23);
    assert_eq!(resp["error"]["code"], -32602);

    // 5. Absent arguments -> -32602
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 24,
        "method": "tools/call",
        "params": {
            "name": "mimori_map"
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 24);
    assert_eq!(resp["error"]["code"], -32602);

    // 6. Arguments as string -> -32602
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 25,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": "not-an-object"
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 25);
    assert_eq!(resp["error"]["code"], -32602);

    // 7. Wrong field type (limit as string) -> -32602
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 26,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": {
                "limit": "three"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 26);
    assert_eq!(resp["error"]["code"], -32602);

    // 8. Execution failure (unresolvable symbol coordinate) -> isError: true, not -32602
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 27,
        "method": "tools/call",
        "params": {
            "name": "mimori_slice",
            "arguments": {
                "coordinate": "nonexistent_symbol"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 27);
    assert_eq!(resp["result"]["isError"], true);

    // 9. Unknown JSON-RPC method -> -32601
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 28,
        "method": "custom/unknown"
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 28);
    assert!(resp["error"].is_object());
    assert_eq!(resp["error"]["code"], -32601);

    // 10. Malformed JSON -> -32700
    let stdin = client.child.stdin.as_mut().unwrap();
    stdin.write_all(b"not a valid json\n").unwrap();
    stdin.flush().unwrap();
    let resp = client.recv();
    assert!(resp["error"].is_object());
    assert_eq!(resp["error"]["code"], -32700);
}

#[test]
fn test_mcp_path_confinement_line_coordinates() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let in_workspace_file = src.join("inside.rs");
    fs::write(
        &in_workspace_file,
        "pub fn inside() { println!(\"secret\"); }",
    )
    .unwrap();

    let mut client = McpClient::spawn(dir.path());

    // 1. Hostile outside absolute path -> -32602 InvalidParams
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 40,
        "method": "tools/call",
        "params": {
            "name": "mimori_slice",
            "arguments": {
                "coordinate": "/etc/passwd:#L1-5"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 40);
    assert_eq!(resp["error"]["code"], -32602);
    let msg = resp["error"]["message"].as_str().unwrap();
    assert!(msg.contains("escapes workspace") || msg.contains("Cannot resolve path"));
    assert!(!msg.contains("root:x:0:0"));

    // 2. Relative ../ escape -> -32602
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 41,
        "method": "tools/call",
        "params": {
            "name": "mimori_slice",
            "arguments": {
                "coordinate": "../outside.rs:#L1-5"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 41);
    assert_eq!(resp["error"]["code"], -32602);

    // 3. In-workspace absolute path -> succeeds
    let abs_str = in_workspace_file.to_str().unwrap();
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": {
            "name": "mimori_slice",
            "arguments": {
                "coordinate": format!("{}:#L1-1", abs_str)
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 42);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pub fn inside"));
}

#[test]
fn test_mcp_workspace_dir_confinement() {
    let dir = tempdir().unwrap();
    let hostile_dir = tempdir().unwrap();
    let hostile_probe = hostile_dir.path().join("probe");

    let mut client = McpClient::spawn(dir.path());

    // 1. Hostile absolute workspace_dir -> -32602, and no .mimori created outside
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 50,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": {
                "workspace_dir": hostile_probe.to_str().unwrap()
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 50);
    assert_eq!(resp["error"]["code"], -32602);
    assert!(
        !hostile_probe.join(".mimori").exists(),
        "hostile .mimori must not be created"
    );

    // 2. Relative escaping workspace_dir -> -32602
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 51,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": {
                "workspace_dir": "../outside"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 51);
    assert_eq!(resp["error"]["code"], -32602);

    // 3. In-workspace relative workspace_dir -> succeeds
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("lib.rs"), "pub fn hello() {}").unwrap();

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 52,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": {
                "workspace_dir": "src"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 52);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("hello"),
        "mimori_map in scoped workspace_dir must contain symbols: {}",
        text
    );
}

#[test]
fn test_mcp_find_limit_and_bounded_output() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    let mut code = String::new();
    for i in 0..100 {
        code.push_str(&format!("pub fn sample_token_fn_{}() {{}}\n", i));
    }
    fs::write(src.join("tokens.rs"), &code).unwrap();

    let mut client = McpClient::spawn(dir.path());

    // 1. Explicit limit
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 60,
        "method": "tools/call",
        "params": {
            "name": "mimori_find",
            "arguments": {
                "pattern": "sample_token_fn",
                "limit": 5
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 60);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("5 matches of 100 [--limit]"));

    // 2. Limit 0 returns empty valid result
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 61,
        "method": "tools/call",
        "params": {
            "name": "mimori_find",
            "arguments": {
                "pattern": "sample_token_fn",
                "limit": 0
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 61);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("0 matches of 100 [--limit]"));

    // 3. Default cap when limit is omitted
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 62,
        "method": "tools/call",
        "params": {
            "name": "mimori_find",
            "arguments": {
                "pattern": "sample_token_fn"
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 62);
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("50 matches of 100 [--limit]"));
    assert!(text.len() < 51200, "Output must stay well under 51.2KB");

    // 4. mimori_map limit 0 and usize::MAX
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 63,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": {
                "limit": 0
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 63);
    assert_eq!(resp["result"]["isError"], false);

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 64,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": {
                "limit": usize::MAX
            }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], 64);
    assert_eq!(resp["result"]["isError"], false);
}

#[test]
fn test_mcp_request_cancellation() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("work.rs"), "pub fn heavy_work() {}").unwrap();

    let mut client = McpClient::spawn(dir.path());

    // Send request 100, then cancel it immediately
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": { "limit": 10 }
        }
    }));
    client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {
            "requestId": 100,
            "reason": "user aborted"
        }
    }));

    // Send a subsequent ping with id 101
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 101,
        "method": "ping"
    }));

    // The next response must be for ping 101, id 100 was suppressed
    let resp = client.recv();
    assert_eq!(
        resp["id"], 101,
        "Expected response for id 101, but got: {:?}",
        resp
    );
}

#[test]
fn test_mcp_protocol_version_negotiation() {
    let dir = tempdir().unwrap();
    let mut client = McpClient::spawn(dir.path());

    // 1. Exact version request
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05"
        }
    }));
    let resp1 = client.recv();
    assert_eq!(resp1["result"]["protocolVersion"], "2024-11-05");

    // 2. Modern version request -> returns 2024-11-05 for fallback
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "initialize",
        "params": {
            "protocolVersion": "2026-07-28"
        }
    }));
    let resp2 = client.recv();
    assert_eq!(resp2["result"]["protocolVersion"], "2024-11-05");
}

#[test]
fn test_mcp_whole_session_stdout_purity() {
    let dir = tempdir().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("code.rs"), "pub fn demo() {}").unwrap();

    let mut client = McpClient::spawn(dir.path());

    // 1. initialize
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "init_1",
        "method": "initialize",
        "params": { "protocolVersion": "2024-11-05" }
    }));
    let r1 = client.recv();
    assert_eq!(r1["id"], "init_1");

    // 2. ping
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "ping_2",
        "method": "ping"
    }));
    let r2 = client.recv();
    assert_eq!(r2["id"], "ping_2");

    // 3. tools/list
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "list_3",
        "method": "tools/list"
    }));
    let r3 = client.recv();
    assert_eq!(r3["id"], "list_3");

    // 4. tools/call valid
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "call_4",
        "method": "tools/call",
        "params": {
            "name": "mimori_find",
            "arguments": { "pattern": "demo" }
        }
    }));
    let r4 = client.recv();
    assert_eq!(r4["id"], "call_4");

    // 5. cancelled call + ping
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "call_cancel_5",
        "method": "tools/call",
        "params": {
            "name": "mimori_map",
            "arguments": {}
        }
    }));
    client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": "call_cancel_5" }
    }));
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "ping_6",
        "method": "ping"
    }));
    let mut r5 = client.recv();
    if r5["id"] == "call_cancel_5" {
        r5 = client.recv();
    }
    assert_eq!(r5["id"], "ping_6");

    // 6. unknown method
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "unknown_7",
        "method": "unknown/route"
    }));
    let r6 = client.recv();
    assert_eq!(r6["id"], "unknown_7");

    // 7. malformed json line
    let stdin = client.child.stdin.as_mut().unwrap();
    stdin.write_all(b"not a valid json line\n").unwrap();
    stdin.flush().unwrap();
    let r7 = client.recv();
    assert_eq!(r7["error"]["code"], -32700);
}

#[test]
fn test_mcp_memory_and_debt_tools() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".agents");
    fs::create_dir_all(&agents_dir).unwrap();

    let memory_md = "\
# Project Memory

## Active Epics & Scale
- Scale: 100 active nodes.

## KNOWN DEBT (open only — one line per item, delete when done)
# Deliberate gaps get ledger lines: - accepted <what> <- <why> -> <trigger>

- accepted test gaps <- daemon loops untested -> deliberate gaps preserved

## Domain Vocabulary & Gotchas
- Gotchas: Keep memory compact.
";
    fs::write(agents_dir.join("memory.md"), memory_md).unwrap();

    let src_dir = dir.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let code_file = src_dir.join("cache.rs");
    fs::write(
        &code_file,
        "// ponytail: bypass cache <- max 100 req/s -> implement redis pool\npub fn get_cache() {}\n",
    )
    .unwrap();

    let mut client = McpClient::spawn(dir.path());

    // 1. Debt list
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "debt_list_1",
        "method": "tools/call",
        "params": {
            "name": "mimori_debt",
            "arguments": { "action": "list" }
        }
    }));
    let resp = client.recv();
    assert_eq!(resp["id"], "debt_list_1");
    let content = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("bypass cache"));
    assert!(content.contains("implement redis pool"));

    // 2. Debt sync
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "debt_sync_2",
        "method": "tools/call",
        "params": {
            "name": "mimori_debt",
            "arguments": { "action": "sync" }
        }
    }));
    let resp2 = client.recv();
    assert_eq!(resp2["id"], "debt_sync_2");
    let text2 = resp2["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text2.contains("DEBT_SYNC:"));

    // 3. Memory lint
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "mem_lint_3",
        "method": "tools/call",
        "params": {
            "name": "mimori_memory",
            "arguments": { "action": "lint" }
        }
    }));
    let resp3 = client.recv();
    assert_eq!(resp3["id"], "mem_lint_3");
    let text3 = resp3["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text3.contains("MEM_LINT:"));
    assert!(text3.contains("exit 0."));

    // 4. Memory show section debt
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "mem_show_4",
        "method": "tools/call",
        "params": {
            "name": "mimori_memory",
            "arguments": { "action": "show", "section": "debt" }
        }
    }));
    let resp4 = client.recv();
    assert_eq!(resp4["id"], "mem_show_4");
    let text4 = resp4["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text4.contains("bypass cache"));
    assert!(text4.contains("accepted test gaps"));

    // 5. Memory resolve
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "mem_resolve_5",
        "method": "tools/call",
        "params": {
            "name": "mimori_memory",
            "arguments": { "action": "resolve", "target": "bypass cache" }
        }
    }));
    let resp5 = client.recv();
    assert_eq!(resp5["id"], "mem_resolve_5");
    let text5 = resp5["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text5.contains("MEM_RESOLVE: deleted 1 lines matching 'bypass cache'"));

    // 6. Debt check
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "debt_check_6",
        "method": "tools/call",
        "params": {
            "name": "mimori_debt",
            "arguments": { "action": "check" }
        }
    }));
    let resp6 = client.recv();
    assert_eq!(resp6["id"], "debt_check_6");
    let text6 = resp6["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text6.contains("DEBT_CHECK:"));
    assert!(text6.contains("exit 0."));
}
