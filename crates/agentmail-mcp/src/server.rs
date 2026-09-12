//! The MCP transport: rmcp over stdio.
//!
//! rmcp was chosen after checking that it can express the three things this server
//! cannot do without — an `experimental` capability map (`claude/channel`), an
//! arbitrary outgoing notification (`ServerNotification::CustomNotification`), and
//! resources with `list_changed`. It can, so there is no hand-rolled JSON-RPC loop
//! here; this file is only shape conversion over `service::Service`.

use std::collections::BTreeMap;
use std::sync::Arc;

use agentmail_core::Harness;
use rmcp::model::*;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::Value;

use crate::service::{
    Service, ServiceError, CHANNEL_CAPABILITY, CHANNEL_METHOD, INSTRUCTIONS, RESOURCE_MIME,
};

/// Claude only enables a channel for servers that declare it, and no other client has
/// any use for the capability, so it is announced only where it means something.
pub fn server_info(harness: &Harness) -> ServerInfo {
    let mut caps = ServerCapabilities::builder()
        .enable_tools()
        .enable_resources()
        .enable_resources_list_changed()
        .build();
    if *harness == Harness::Claude {
        // The builder is a typestate, so the optional capability is set on the value.
        let mut experimental: BTreeMap<String, JsonObject> = BTreeMap::new();
        experimental.insert(CHANNEL_CAPABILITY.to_string(), JsonObject::new());
        caps.experimental = Some(experimental);
    }

    let mut info = ServerInfo::new(caps).with_instructions(INSTRUCTIONS);
    info.server_info = Implementation::new("agentmail", env!("CARGO_PKG_VERSION"));
    info
}

pub fn channel_notification(params: Value) -> ServerNotification {
    ServerNotification::CustomNotification(CustomNotification::new(CHANNEL_METHOD, Some(params)))
}

/// One JSON line per handshake in `<state>/diagnostics.jsonl`, capped so it cannot grow
/// without bound. Diagnostics only: nothing reads it back.
fn record_handshake(request: &InitializeRequestParams) {
    let entry = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "protocol_version": request.protocol_version.as_str(),
        "client": serde_json::to_value(&request.client_info).unwrap_or(Value::Null),
        "capabilities": serde_json::to_value(&request.capabilities).unwrap_or(Value::Null),
    });
    tracing::debug!(%entry, "client handshake");

    let Ok(paths) = agentmail_core::Paths::from_env() else {
        return;
    };
    let path = paths.root().join("diagnostics.jsonl");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > DIAGNOSTICS_MAX) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{entry}");
    }
}

/// 256 KiB of handshakes is thousands of them; past that the file starts over.
const DIAGNOSTICS_MAX: u64 = 256 * 1024;

#[derive(Clone)]
pub struct AgentmailServer {
    svc: Arc<Service>,
}

impl AgentmailServer {
    pub fn new(svc: Arc<Service>) -> Self {
        AgentmailServer { svc }
    }
}

impl ServerHandler for AgentmailServer {
    fn get_info(&self) -> ServerInfo {
        server_info(&self.svc.identity().harness)
    }

    /// Records what the client said about itself before answering.
    ///
    /// Whether Claude actually routes `notifications/claude/channel` to this server
    /// depends on how the session was launched, and nothing in `initialize` obviously
    /// says so. Keeping every handshake makes a channel-enabled session comparable with
    /// a plain one, which is the only way to find out.
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        record_handshake(&request);
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = crate::service::tools()
            .into_iter()
            .map(|t| {
                let schema: JsonObject = serde_json::from_value(t.schema).unwrap_or_default();
                Tool::new(t.name, t.description, Arc::new(schema))
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let args = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(Default::default()));

        match self.svc.call_tool(&request.name, &args).await {
            Ok(value) => Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
            )])
            .into()),
            Err(ServiceError::UnknownTool(name)) => Err(McpError::invalid_params(
                format!("unknown tool {name}"),
                None,
            )),
            Err(ServiceError::BadArguments(msg)) => Err(McpError::invalid_params(msg, None)),
            // A store or delivery failure is the model's problem to work around, not a
            // protocol error: hand it back as a failed tool result it can read.
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(e.to_string())]).into()),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = self
            .svc
            .resources()
            .await
            .into_iter()
            .map(|r| {
                let mut res = Resource::new(r.uri, r.name).with_mime_type(RESOURCE_MIME);
                if let Some(d) = r.description {
                    res = res.with_description(d);
                }
                res
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let templates = self
            .svc
            .resource_templates()
            .into_iter()
            .map(|t| {
                let mut tpl = ResourceTemplate::new(t.uri, t.name).with_mime_type(RESOURCE_MIME);
                if let Some(d) = t.description {
                    tpl = tpl.with_description(d);
                }
                tpl
            })
            .collect();
        Ok(ListResourceTemplatesResult::with_all_items(templates))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        match self.svc.read_resource(&request.uri).await {
            Some(text) => {
                Ok(
                    ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
                        uri: request.uri,
                        mime_type: Some(RESOURCE_MIME.to_string()),
                        text,
                        meta: None,
                    }])
                    .into(),
                )
            }
            None => Err(McpError::resource_not_found(
                format!("no session at {}", request.uri),
                None,
            )),
        }
    }
}
