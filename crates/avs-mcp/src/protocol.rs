//! MCP over stdio: newline-delimited JSON-RPC, tools only.
//!
//! The transport knows nothing about Agent Video Studio. It lists the tool catalog
//! and hands each `tools/call` to a [`ToolHost`].

use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// A failed tool call: a stable code, a message, and structured details.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    pub details: Value,
}

impl ToolError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), details: json!({}) }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

pub trait ToolHost {
    /// The `tools/list` records: `{name, title, description, inputSchema}`.
    fn tools(&self) -> Vec<Value>;
    fn call(&self, name: &str, arguments: &Value) -> Result<Value, ToolError>;
    fn server_info(&self) -> Value;
    fn instructions(&self) -> String;
}

pub fn serve<R: BufRead, W: Write>(host: &dyn ToolHost, reader: R, mut writer: W) -> io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle_input(host, &message),
            Err(error) => Some(rpc_error(Value::Null, -32700, format!("parse error: {error}"))),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut writer, &response).map_err(io::Error::other)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
    Ok(())
}

pub fn handle_input(host: &dyn ToolHost, message: &Value) -> Option<Value> {
    if let Some(batch) = message.as_array() {
        let responses: Vec<Value> = batch.iter().filter_map(|item| handle_request(host, item)).collect();
        return (!responses.is_empty()).then_some(Value::Array(responses));
    }
    handle_request(host, message)
}

fn handle_request(host: &dyn ToolHost, message: &Value) -> Option<Value> {
    // A message without an id is a notification and gets no reply.
    let id = message.get("id")?.clone();
    let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = message.get("params");
    Some(match method {
        "initialize" => {
            let protocol = params.and_then(|value| value.get("protocolVersion")).cloned().unwrap_or_else(|| json!(MCP_PROTOCOL_VERSION));
            rpc_result(id, json!({
                "protocolVersion": protocol,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": host.server_info(),
                "instructions": host.instructions(),
            }))
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({ "tools": host.tools() })),
        "tools/call" => {
            let name = params.and_then(|value| value.get("name")).and_then(Value::as_str).unwrap_or_default();
            let arguments = params.and_then(|value| value.get("arguments")).cloned().unwrap_or_else(|| json!({}));
            rpc_result(id, tool_result(name, host.call(name, &arguments)))
        }
        _ => rpc_error(id, -32601, format!("unknown method: {method}")),
    })
}

/// Every tool answers in one shape. Success: `{ok: true, tool, ...result}`. Failure:
/// `{ok: false, tool, error: {code, message, details}}` with `isError: true`.
pub fn tool_result(name: &str, result: Result<Value, ToolError>) -> Value {
    let (structured, is_error) = match result {
        Ok(Value::Object(mut fields)) => {
            fields.insert("ok".into(), json!(true));
            fields.insert("tool".into(), json!(name));
            (Value::Object(fields), false)
        }
        Ok(other) => (json!({ "ok": true, "tool": name, "result": other }), false),
        Err(error) => (json!({ "ok": false, "tool": name, "error": { "code": error.code, "message": error.message, "details": error.details } }), true),
    };
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string()) }],
        "structuredContent": structured,
        "isError": is_error,
    })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
