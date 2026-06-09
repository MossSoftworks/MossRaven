//! MCP (Model Context Protocol) control-surface server.
//!
//! Exposes the *outer* search API as MCP tools so Tier-1 drivers (Claude Code,
//! Cowork, the WPF shell) can drive the engine over JSON-RPC. The autonomous
//! inner loop (mutate → surrogate-prune → PoB-sim → archive-place) runs inside
//! the service and is Claude-free; it does NOT go through this MCP surface.
//!
//! ## Transport — newline-delimited JSON-RPC 2.0 over stdio
//!
//! Each message is one JSON object terminated by `\n`. This matches what
//! Claude Code and the MCP CLI clients send by default. Stdout carries the
//! JSON stream; tracing logs go to stderr (enforced by the service binary).
//!
//! ## Lifecycle (per MCP spec)
//!
//! 1. Client sends `initialize` → server responds with capabilities.
//! 2. Client sends `notifications/initialized` (no response).
//! 3. Client calls `tools/list` to discover available tools.
//! 4. Client calls `tools/call` with name + arguments per tool.
//! 5. Optional `ping` for keepalive.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Error)]
pub enum McpError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("tool not found: {0}")]
    UnknownTool(String),
    #[error("tool execution failed: {0}")]
    ToolFailed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// The control-surface tool list. Stable IDs — referenced by Claude Code prompts.
pub fn control_surface_tools() -> Vec<ToolSchema> {
    vec![
        ToolSchema {
            name: "seed_hypothesis".into(),
            description: "Begin a new MAP-Elites search from a free-text concept. Returns the structured hypothesis the engine will explore.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "concept": { "type": "string", "description": "Free-text build idea, e.g. 'cold DoT scaled through obscure ailment'" } },
                "required": ["concept"]
            }),
        },
        ToolSchema {
            name: "run_search".into(),
            description: "Run N generations of the surrogate-prune + PoB-sim inner loop. Returns per-generation reports + a summary.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "generations": { "type": "integer", "minimum": 1, "description": "How many inner-loop generations to run" },
                    "region": { "type": "string", "description": "Optional cell-coord filter (e.g. 'chaos/es/boss/*')" }
                },
                "required": ["generations"]
            }),
        },
        ToolSchema {
            name: "read_archive".into(),
            description: "Snapshot the full MAP-Elites archive. Each entry is (coords, build).".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolSchema {
            name: "inspect_cell".into(),
            description: "Get the current elite build for a specific archive cell.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "damage_type":     { "type": "string" },
                    "defense_layer":   { "type": "string" },
                    "role":            { "type": "string" },
                    "scaling_vector":  { "type": "string" }
                },
                "required": ["damage_type", "defense_layer", "role", "scaling_vector"]
            }),
        },
        ToolSchema {
            name: "get_frontier".into(),
            description: "Pareto frontier across novelty × power × cost.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
    ]
}

/// Trait implemented by the orchestration core. The MCP transport layer wraps
/// this so we can unit-test the tool handlers without standing up JSON-RPC.
#[async_trait]
pub trait ControlSurface: Send + Sync {
    async fn seed_hypothesis(&self, concept: &str) -> Result<Value, McpError>;
    async fn run_search(&self, generations: u32, region: Option<String>) -> Result<Value, McpError>;
    async fn read_archive(&self) -> Result<Value, McpError>;
    async fn inspect_cell(&self, coords: Value) -> Result<Value, McpError>;
    async fn get_frontier(&self) -> Result<Value, McpError>;
}

// ----- JSON-RPC 2.0 wire types -----

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

// ----- The serve loop -----

/// Drive a stdio JSON-RPC 2.0 session against a [`ControlSurface`].
///
/// Reads newline-delimited JSON from stdin, dispatches, writes responses
/// to stdout one-per-line. Returns when stdin closes (EOF).
pub async fn serve_stdio<S: ControlSurface>(surface: S) -> Result<(), McpError> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    tracing::info!("MCP stdio server ready (newline-delimited JSON-RPC 2.0)");

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, line = %line, "could not parse JSON-RPC request");
                continue;
            }
        };
        if !req.jsonrpc.is_empty() && req.jsonrpc != "2.0" {
            tracing::warn!(jsonrpc = %req.jsonrpc, "non-2.0 jsonrpc version; processing anyway");
        }

        // Notifications (no `id`) don't get a response.
        let is_notification = req.id.is_none();
        let id = req.id.unwrap_or(Value::Null);

        let result = dispatch(&surface, &req.method, &req.params).await;

        if is_notification {
            if let Err(e) = &result {
                tracing::debug!(method = %req.method, error = %e, "notification handler returned error (no response sent)");
            }
            continue;
        }

        let response = match result {
            Ok(v) => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(v),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: error_code(&e),
                    message: e.to_string(),
                }),
            },
        };

        let json = serde_json::to_string(&response)
            .map_err(|e| McpError::Protocol(format!("could not serialize response: {e}")))?;
        stdout.write_all(json.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }

    tracing::info!("MCP stdio server: stdin EOF, shutting down");
    Ok(())
}

fn error_code(err: &McpError) -> i32 {
    match err {
        McpError::UnknownTool(_) => -32601,   // Method not found
        McpError::Protocol(_) => -32600,      // Invalid Request
        McpError::Io(_) => -32603,            // Internal error
        McpError::ToolFailed(_) => -32000,    // Server error
    }
}

async fn dispatch<S: ControlSurface>(
    surface: &S,
    method: &str,
    params: &Value,
) -> Result<Value, McpError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "mossraven-service",
                "version": env!("CARGO_PKG_VERSION"),
            }
        })),
        "notifications/initialized" => Ok(Value::Null),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": control_surface_tools() })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::Protocol("tools/call missing 'name'".into()))?;
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Null);
            let result = call_tool(surface, name, args).await?;
            // MCP wraps tool results in a content array.
            Ok(json!({
                "content": [
                    {
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result).unwrap_or_default(),
                    }
                ],
                "isError": false,
                "structuredContent": result,
            }))
        }
        other => Err(McpError::UnknownTool(other.to_string())),
    }
}

async fn call_tool<S: ControlSurface>(
    surface: &S,
    name: &str,
    args: Value,
) -> Result<Value, McpError> {
    match name {
        "seed_hypothesis" => {
            let concept = args
                .get("concept")
                .and_then(|v| v.as_str())
                .ok_or_else(|| McpError::Protocol("seed_hypothesis missing 'concept'".into()))?;
            surface.seed_hypothesis(concept).await
        }
        "run_search" => {
            let generations = args
                .get("generations")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| McpError::Protocol("run_search missing 'generations'".into()))?
                as u32;
            let region = args.get("region").and_then(|v| v.as_str()).map(String::from);
            surface.run_search(generations, region).await
        }
        "read_archive" => surface.read_archive().await,
        "inspect_cell" => surface.inspect_cell(args).await,
        "get_frontier" => surface.get_frontier().await,
        other => Err(McpError::UnknownTool(other.to_string())),
    }
}
