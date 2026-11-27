use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

fn deserialize_present_id<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    /// `None` when omitted (JSON-RPC notification); `Some(Value::Null)` when
    /// explicitly `"id": null` (invalid MCP request ID).
    #[serde(default, deserialize_with = "deserialize_present_id")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 / MCP request IDs must be strings or numbers (never null, bool, array, or object).
pub fn is_valid_request_id(id: &Value) -> bool {
    matches!(id, Value::String(_) | Value::Number(_))
}

impl JsonRpcRequest {
    /// Parse a raw JSON-RPC 2.0 frame payload, distinguishing `-32700` (Parse Error)
    /// from `-32600` (Invalid Request) while preserving valid request `id` values.
    pub fn parse_json(payload: &str) -> Result<Self, Box<JsonRpcResponse>> {
        let val: Value = serde_json::from_str(payload).map_err(|e| {
            Box::new(JsonRpcResponse::error(
                Value::Null,
                -32700,
                format!("Parse error: {}", e),
            ))
        })?;

        let Value::Object(mut obj) = val else {
            return Err(Box::new(JsonRpcResponse::error(
                Value::Null,
                -32600,
                "Invalid Request: expected a JSON object",
            )));
        };

        let id = match obj.remove("id") {
            None => None,
            Some(Value::Null) => {
                return Err(Box::new(JsonRpcResponse::error(
                    Value::Null,
                    -32600,
                    "Invalid Request: 'id' must not be null",
                )));
            }
            Some(v) if is_valid_request_id(&v) => Some(v),
            Some(_) => {
                return Err(Box::new(JsonRpcResponse::error(
                    Value::Null,
                    -32600,
                    "Invalid Request: 'id' must be a string or number",
                )));
            }
        };
        let err_id = id.clone().unwrap_or(Value::Null);

        let jsonrpc = match obj.remove("jsonrpc") {
            Some(Value::String(s)) if s == "2.0" => s,
            Some(Value::String(other)) => {
                return Err(Box::new(JsonRpcResponse::error(
                    err_id,
                    -32600,
                    format!("Invalid Request: expected jsonrpc '2.0', got '{}'", other),
                )));
            }
            _ => {
                return Err(Box::new(JsonRpcResponse::error(
                    err_id,
                    -32600,
                    "Invalid Request: 'jsonrpc' must be '2.0'",
                )));
            }
        };

        let method = match obj.remove("method") {
            Some(Value::String(m)) if !m.is_empty() => m,
            _ => {
                return Err(Box::new(JsonRpcResponse::error(
                    err_id,
                    -32600,
                    "Invalid Request: 'method' is required and must be a non-empty string",
                )));
            }
        };

        let params = obj.remove("params");
        Ok(Self {
            jsonrpc,
            id,
            method,
            params,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Invalid params: {0}")]
    InvalidParams(String),
    #[error("Unknown tool: '{0}'")]
    NotFound(String),
    #[error("{0}")]
    Execution(String),
}
