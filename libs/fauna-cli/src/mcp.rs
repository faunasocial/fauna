//! The MCP method layer: JSON-RPC 2.0 messages in, JSON-RPC 2.0 messages out,
//! over whatever transport the shell runs (`apps/cli.md` § Test surface).
//!
//! **A minimal in-tree layer, deliberately.** It serves `initialize`, `ping`,
//! `tools/list` and `tools/call`, and beside them the read-only resources
//! (`resources/list`, `resources/templates/list`, `resources/read`; no
//! subscriptions) and the coaching prompts (`prompts/list`, `prompts/get`) —
//! and ignores every notification, which is the whole of what
//! the stdio transport asks of a server. The official Rust SDK is the other
//! shape cli.md names; whether it replaces this is the owner's decision at a
//! dependency round, and the replacement is a change to this file and
//! [`crate::stdio`] only — the catalog and its handlers do not see the
//! protocol layer.
//!
//! Batches are refused (`-32600`): no revision this layer negotiates allows
//! them.

use serde_json::{Value, json};

use crate::catalog::{self, CATALOG, PROMPTS, Prompt, RESOURCES, Resource, Tool};
use crate::tools::{self, Host};

/// The protocol revisions this layer speaks, newest first. A client asking
/// for one of them gets it back; any other request is answered with the
/// newest, and the client decides (the spec's negotiation rule).
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
/// MCP's own code for a `resources/read` of a URI the server has no resource
/// at.
const RESOURCE_NOT_FOUND: i64 = -32002;

/// One MCP server over one [`Host`].
pub struct Server<H> {
    host: H,
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn listed(t: &Tool) -> Value {
    let a = t.annotations;
    json!({
        "name": t.name,
        "title": t.title,
        "description": t.description,
        "inputSchema": t.schema(),
        "annotations": {
            "title": t.title,
            "readOnlyHint": a.read_only,
            "destructiveHint": a.destructive,
            "idempotentHint": a.idempotent,
            "openWorldHint": a.open_world,
        },
    })
}

/// One `resources/list` or `resources/templates/list` entry.
fn listed_resource(r: &Resource) -> Value {
    let key = if r.template { "uriTemplate" } else { "uri" };
    json!({
        key: r.uri,
        "name": r.name,
        "title": r.title,
        "description": r.description,
        "mimeType": r.mime_type,
    })
}

/// One `prompts/list` entry.
fn listed_prompt(p: &Prompt) -> Value {
    json!({
        "name": p.name,
        "title": p.title,
        "description": p.description,
        "arguments": p.arguments.iter().map(|a| json!({
            "name": a.name,
            "description": a.description,
            "required": a.required,
        })).collect::<Vec<_>>(),
    })
}

/// `prompts/get`: the coaching text for the arguments, one user message. A
/// prompt reads nothing from the nest, so it needs no session.
fn prompts_get(id: Value, params: &Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error(id, INVALID_PARAMS, "prompts/get needs a prompt name");
    };
    let Some(prompt) = catalog::prompt(name) else {
        return error(id, INVALID_PARAMS, format!("unknown prompt: {name}"));
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match (prompt.text)(&args) {
        Ok(text) => result(
            id,
            json!({
                "description": prompt.description,
                "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
            }),
        ),
        Err(why) => error(id, INVALID_PARAMS, why),
    }
}

impl<H: Host> Server<H> {
    pub fn new(host: H) -> Self {
        Self { host }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    /// Handle one inbound message (one line of the stdio transport). `None`
    /// for a notification, which takes no answer.
    pub async fn handle(&self, message: &str) -> Option<Value> {
        let parsed: Value = match serde_json::from_str(message) {
            Ok(v) => v,
            Err(e) => return Some(error(Value::Null, PARSE_ERROR, format!("parse error: {e}"))),
        };
        if parsed.is_array() {
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "batches are not supported",
            ));
        }
        let Some(method) = parsed.get("method").and_then(Value::as_str) else {
            // A response to something we never sent, or garbage: a message
            // with an id gets the refusal, one without is dropped.
            return parsed
                .get("id")
                .map(|id| error(id.clone(), INVALID_REQUEST, "not a request"));
        };
        // A request carries an id; a notification does not and gets nothing back.
        let id = parsed.get("id")?.clone();
        let params = parsed.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => result(id, self.initialize(&params)),
            "ping" => result(id, json!({})),
            "tools/list" => result(
                id,
                json!({ "tools": CATALOG.iter().map(listed).collect::<Vec<_>>() }),
            ),
            "tools/call" => self.tools_call(id, &params).await,
            "resources/list" => result(
                id,
                json!({ "resources": RESOURCES.iter().filter(|r| !r.template).map(listed_resource).collect::<Vec<_>>() }),
            ),
            "resources/templates/list" => result(
                id,
                json!({ "resourceTemplates": RESOURCES.iter().filter(|r| r.template).map(listed_resource).collect::<Vec<_>>() }),
            ),
            "resources/read" => self.resources_read(id, &params).await,
            "prompts/list" => result(
                id,
                json!({ "prompts": PROMPTS.iter().map(listed_prompt).collect::<Vec<_>>() }),
            ),
            "prompts/get" => prompts_get(id, &params),
            other => error(id, METHOD_NOT_FOUND, format!("method not found: {other}")),
        })
    }

    /// `resources/read`: the contents as one JSON text block. A refusal is a
    /// JSON-RPC error carrying the nest's code verbatim — a resource has no
    /// `isError` result shape.
    async fn resources_read(&self, id: Value, params: &Value) -> Value {
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return error(id, INVALID_PARAMS, "resources/read needs a uri");
        };
        match tools::read_resource(&self.host, uri).await {
            None => error(id, RESOURCE_NOT_FOUND, format!("no resource at {uri}")),
            Some(Ok((resource, data))) => result(
                id,
                json!({ "contents": [{
                    "uri": uri,
                    "mimeType": resource.mime_type,
                    "text": data.to_string(),
                }] }),
            ),
            Some(Err(e)) => error(id, INTERNAL_ERROR, e.text()),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = asked
            .filter(|v| PROTOCOL_VERSIONS.contains(v))
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "listChanged": false, "subscribe": false },
                "prompts": { "listChanged": false },
            },
            "serverInfo": { "name": "fauna-cli", "version": env!("CARGO_PKG_VERSION") },
        })
    }

    async fn tools_call(&self, id: Value, params: &Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error(id, INVALID_PARAMS, "tools/call needs a tool name");
        };
        let Some(tool) = catalog::tool(name) else {
            return error(id, INVALID_PARAMS, format!("unknown tool: {name}"));
        };
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match tools::call(&self.host, tool, &args).await {
            Ok(data) => result(
                id,
                json!({
                    "content": [{ "type": "text", "text": data.to_string() }],
                    "structuredContent": data,
                    "isError": false,
                }),
            ),
            Err(e) => result(
                id,
                json!({
                    "content": [{ "type": "text", "text": e.text() }],
                    "isError": true,
                }),
            ),
        }
    }
}
