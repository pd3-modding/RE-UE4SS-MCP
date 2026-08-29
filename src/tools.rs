//! The MCP tool surface, and the axum server that hosts it.
//!
//! Tool handlers must never block a tokio worker for long: `Host::lua_eval` blocks until the
//! game thread runs the code, so every call into the host goes through `spawn_blocking`.

use crate::ffi::Host;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LuaEvalArgs {
    /// Lua source to execute inside the game. Use `return` to get a value back; anything
    /// printed is captured too.
    pub code: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LogTailArgs {
    /// How many lines from the end of UE4SS.log. Defaults to 50.
    #[serde(default)]
    pub lines: Option<u32>,
    /// Only return lines containing this substring.
    #[serde(default)]
    pub filter: Option<String>,
}

/// Number of live MCP sessions, so the log can say how many clients remain.
static ACTIVE_SESSIONS: AtomicU64 = AtomicU64::new(0);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// Logs "connected" on construction and "disconnected" when the last clone of the owning
/// `Pd3Server` goes away.
///
/// This is held behind an `Arc` rather than being dropped with `Pd3Server` directly, because
/// rmcp clones the handler internally -- a bare `Drop` impl on `Pd3Server` would report a
/// disconnect every time a clone went out of scope.
struct SessionGuard {
    host: Host,
    id: u64,
}

impl SessionGuard {
    fn new(host: Host) -> Self {
        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let active = ACTIVE_SESSIONS.fetch_add(1, Ordering::SeqCst) + 1;
        host.log(&format!(
            "[MCP] client connected (session {id}, {active} active)"
        ));
        Self { host, id }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let active = ACTIVE_SESSIONS
            .fetch_sub(1, Ordering::SeqCst)
            .saturating_sub(1);
        self.host.log(&format!(
            "[MCP] client disconnected (session {}, {} active)",
            self.id, active
        ));
    }
}

#[derive(Clone)]
pub struct Pd3Server {
    host: Host,
    /// Kept alive for the lifetime of the session purely for its `Drop`.
    _session: Arc<SessionGuard>,
    pub tool_router: ToolRouter<Pd3Server>,
}

#[tool_router]
impl Pd3Server {
    pub fn new(host: Host) -> Self {
        Self {
            host,
            _session: Arc::new(SessionGuard::new(host)),
            tool_router: Self::tool_router(),
        }
    }

    /// Run `f` off the tokio worker, since host callbacks block on the game thread.
    async fn offload<T, F>(&self, f: F) -> Result<T, ErrorData>
    where
        F: FnOnce(Host) -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        let host = self.host;
        let joined = tokio::task::spawn_blocking(move || f(host)).await;
        match joined {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(ErrorData::internal_error(e, None)),
            Err(e) => Err(ErrorData::internal_error(
                format!("host call panicked or was cancelled: {e}"),
                None,
            )),
        }
    }

    #[tool(
        description = "Execute Lua inside the running game on the game thread, returning \
                       captured output and the returned value. This is the primary way to \
                       inspect and manipulate live game state: FindAllOf, property reads, \
                       UFunction calls. Use `return <expr>` to get a value back."
    )]
    pub async fn lua_eval(
        &self,
        Parameters(args): Parameters<LuaEvalArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let code = args.code.clone();
        let result = self.offload(move |h| h.lua_eval(&code)).await;
        let ok = result.is_ok();
        self.host.on_tool_call("lua_eval", &args.code, ok);
        Ok(CallToolResult::success(vec![ContentBlock::text(
            match result {
                Ok(text) => text,
                Err(e) => return Err(e),
            },
        )]))
    }

    #[tool(
        description = "Loader and game state as JSON: UE4SS version, whether a world/heist is \
                       loaded, which mods are running, and whether UFunction dispatch \
                       (ProcessEvent) resolved correctly on this build."
    )]
    pub async fn game_status(&self) -> Result<CallToolResult, ErrorData> {
        let result = self.offload(|h| h.game_status()).await;
        self.host.on_tool_call("game_status", "{}", result.is_ok());
        Ok(CallToolResult::success(vec![ContentBlock::text(result?)]))
    }

    #[tool(
        description = "Tail UE4SS.log, optionally filtered by a substring. Use this to read \
                       mod output and diagnose errors instead of asking the user to paste it."
    )]
    pub async fn log_tail(
        &self,
        Parameters(args): Parameters<LogTailArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let lines = args.lines.unwrap_or(50);
        let filter = args.filter.clone();
        let result = self
            .offload(move |h| h.log_tail(lines, filter.as_deref()))
            .await;
        self.host.on_tool_call(
            "log_tail",
            &serde_json::to_string(&serde_json::json!({
                "lines": lines,
                "filter": args.filter,
            }))
            .unwrap_or_default(),
            result.is_ok(),
        );
        Ok(CallToolResult::success(vec![ContentBlock::text(result?)]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Pd3Server {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo and Implementation are #[non_exhaustive], so they cannot be built with a
        // struct literal from outside rmcp. Start from Default and assign.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Live interface to a running PAYDAY 3 process via UE4SS. `lua_eval` runs Lua on \
             the game thread; `game_status` reports loader state; `log_tail` reads UE4SS.log. \
             Game objects only exist while a world is loaded -- check game_status before \
             assuming a heist is running."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("ue4ss-pd3", env!("CARGO_PKG_VERSION"));
        info
    }
}

/// Serve MCP over HTTP until `shutdown` fires.
pub async fn serve(
    bind: String,
    host: Host,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = StreamableHttpService::new(
        move || Ok(Pd3Server::new(host)),
        LocalSessionManager::default().into(),
        Default::default(),
    );

    let app = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(&bind).await?;

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await?;

    Ok(())
}
