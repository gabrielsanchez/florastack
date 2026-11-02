//! Dynamic MCP tools from the registry, using the official Rust SDK's stdio transport and lifecycle.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use florastack_engine::{Engine, ErrorInfo, Value, automation, branding, control, json};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListResourcesResult, ListToolsResult,
        PaginatedRequestParams, ProgressNotificationParam, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ServerCapabilities,
        ServerConfig, Tool,
    },
    service::{RequestContext, RoleServer},
};

use crate::client::Failure;

#[derive(Clone)]
enum Backend {
    Headless(Arc<Mutex<Engine>>),
    Desktop(control::Client),
}

impl Backend {
    async fn request(&self, method: &str, params: Value) -> Result<Value, ErrorInfo> {
        match self {
            Self::Headless(engine) => {
                let (engine, method) = (engine.clone(), method.to_owned());
                tokio::task::spawn_blocking(move || {
                    let mut engine = engine.lock().map_err(|_| control::error("internal", "engine lock was poisoned"))?;
                    automation::request(&mut engine, &method, params).map_err(|e| e.info())
                })
                .await
                .map_err(|_| control::error("internal", "engine worker stopped"))?
            }
            Self::Desktop(client) => client.request(method, params).await.map_err(|e| control::error("control", &e.to_string()))?,
        }
    }
}

struct Server {
    backend: Backend,
    tools: Vec<Tool>,
}

impl Server {
    async fn new(backend: Backend) -> Result<Self, ErrorInfo> {
        let commands = backend.request("commands.list", json!({})).await?;
        let mut tools = Vec::new();
        for command in commands.as_array().ok_or_else(|| control::error("internal", "registry returned no command list"))? {
            let name = command.get("name").and_then(Value::as_str).ok_or_else(|| control::error("internal", "command has no name"))?;
            // JSON schemas, titles and descriptions are passed through, including local $defs and required fields.
            let tool = json!({
                "name": name, "title": command.get("title"),
                "description": format!("{}\n\n{}", command["summary"].as_str().unwrap_or_default(), command["help"].as_str().unwrap_or_default()),
                "inputSchema": command.get("params"), "outputSchema": command.get("result"),
                "annotations": {"readOnlyHint": command["kind"] == "query"}
            });
            tools.push(serde_json::from_value(tool).map_err(|e| control::error("internal", &e.to_string()))?);
        }
        Ok(Self { backend, tools })
    }

    async fn run(&self, request: CallToolRequestParams, context: &RequestContext<RoleServer>) -> Result<Value, ErrorInfo> {
        let dispatched = self.backend.request("command.dispatch", json!({"command": request.name, "params": request.arguments})).await?;
        let Some(job) = dispatched.get("job").and_then(Value::as_u64) else {
            return Ok(dispatched.get("result").cloned().unwrap_or(Value::Null));
        };
        loop {
            if context.ct.is_cancelled() {
                let _ = self.backend.request("command.dispatch", json!({"command": "job.cancel", "params": {"job": job}})).await;
                return Err(control::error("cancelled", "tool call cancelled"));
            }
            let status = self.backend.request("job.get", json!({"job": job})).await?;
            match status.get("state").and_then(Value::as_str) {
                Some("done") => return Ok(status.get("result").cloned().unwrap_or(Value::Null)),
                Some("failed" | "cancelled") => {
                    return Err(status
                        .get("error")
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_else(|| control::error("cancelled", "job cancelled")));
                }
                _ => {}
            }
            if let Some(token) = context.meta.get("progressToken").cloned().and_then(|v| serde_json::from_value(v).ok()) {
                let mut progress = ProgressNotificationParam::new(token, status["progress"]["fraction"].as_f64().unwrap_or(0.0));
                progress.total = Some(1.0);
                progress.message = status["progress"]["stage"].as_str().map(str::to_owned);
                let _ = context.peer.notify_progress(progress).await;
            }
            tokio::select! {
                () = context.ct.cancelled() => {},
                () = tokio::time::sleep(Duration::from_millis(25)) => {},
            }
        }
    }
}

fn protocol_error(error: ErrorInfo) -> ErrorData {
    let data = json!(error);
    ErrorData::invalid_params(error.message, Some(data))
}

fn no_cursor(request: Option<PaginatedRequestParams>) -> Result<(), ErrorData> {
    if request.is_some_and(|p| p.cursor.is_some()) {
        return Err(ErrorData::invalid_params("this list has no continuation cursor", None));
    }
    Ok(())
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new(branding::CLI_BINARY, env!("CARGO_PKG_VERSION")))
    }

    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> {
        std::future::ready(no_cursor(request).map(|()| {
            let mut result = ListToolsResult::default();
            result.tools.clone_from(&self.tools);
            result
        }))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }

    async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        if self.get_tool(&request.name).is_none() {
            return Err(ErrorData::invalid_params("unknown registry command", None));
        }
        let result = match self.run(request, &context).await {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => CallToolResult::structured_error(json!(error)),
        };
        Ok(result.into())
    }

    async fn list_resources(&self, request: Option<PaginatedRequestParams>, _: RequestContext<RoleServer>) -> Result<ListResourcesResult, ErrorData> {
        no_cursor(request)?;
        let value = self.backend.request("resources.list", json!({})).await.map_err(protocol_error)?;
        serde_json::from_value(value).map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    async fn read_resource(&self, request: ReadResourceRequestParams, _: RequestContext<RoleServer>) -> Result<ReadResourceResponse, ErrorData> {
        let value = self.backend.request("resources.read", json!({"uri": request.uri})).await.map_err(protocol_error)?;
        let result: ReadResourceResult = serde_json::from_value(value).map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        // Use SDK defaults for the negotiated protocol's completion and cache metadata; state is always fresh.
        Ok(ReadResourceResult::new(result.contents).with_ttl_ms(0).with_cache_scope(CacheScope::Private).into())
    }
}

pub fn serve(engine: Engine, connect: Option<&str>, token_file: Option<&std::path::Path>) -> Result<(), Failure> {
    let backend = match (connect, token_file) {
        (Some(address), Some(path)) => Backend::Desktop(
            control::Client::new(control::address(address).map_err(failure)?, control::read_token(path).map_err(failure)?).map_err(failure)?,
        ),
        (None, None) => Backend::Headless(Arc::new(Mutex::new(engine))),
        _ => return Err(Failure::usage("--connect and --token-file must be given together")),
    };
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        let server = Server::new(backend).await.map_err(|info| Failure { exit: 1, info })?;
        let running = server.serve(rmcp::transport::stdio()).await.map_err(failure)?;
        running.waiting().await.map_err(failure)?;
        Ok(())
    })
}

fn failure(error: impl std::fmt::Display) -> Failure {
    Failure::new(1, "mcp", error.to_string())
}
