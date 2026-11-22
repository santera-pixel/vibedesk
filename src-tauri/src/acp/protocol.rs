use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const JSONRPC: &str = "2.0";

/// A request we send to the agent.
#[derive(Debug, Serialize)]
pub struct OutgoingRequest<'a> {
    pub jsonrpc: &'static str,
    pub id: i64,
    pub method: &'a str,
    pub params: Value,
}

impl<'a> OutgoingRequest<'a> {
    pub fn new(id: i64, method: &'a str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC,
            id,
            method,
            params,
        }
    }
}

/// A notification we send to the agent (no id, no response expected).
#[derive(Debug, Serialize)]
pub struct OutgoingNotification<'a> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    pub params: Value,
}

impl<'a> OutgoingNotification<'a> {
    pub fn new(method: &'a str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC,
            method,
            params,
        }
    }
}

/// Anything the agent writes on stdout.
#[derive(Debug, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
            data: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }
}

/// Build a JSON-RPC success response for an agent-initiated request.
pub fn success_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": JSONRPC, "id": id, "result": result })
}

/// Build a JSON-RPC error response for an agent-initiated request.
pub fn error_response(id: &Value, error: RpcError) -> Value {
    json!({ "jsonrpc": JSONRPC, "id": id, "error": error })
}

/// The client capabilities VibeDesk advertises during `initialize`.
///
/// `fs` and `terminal` are required: Vibe routes its file tools and shell
/// tools through the client when the client claims them. `elicitation.form`
/// lets the agent ask structured questions instead of failing.
pub fn client_capabilities() -> Value {
    json!({
        "fs": { "readTextFile": true, "writeTextFile": true },
        "terminal": true,
        "elicitation": { "form": {} },
        "plan": {}
    })
}

pub fn client_info() -> Value {
    json!({
        "name": "vibedesk",
        "title": "VibeDesk",
        "version": env!("CARGO_PKG_VERSION")
    })
}
