//! MCP server hosted inside UE4SS.
//!
//! Exposes the running game to an MCP client (Claude Code) over HTTP, so tools can evaluate
//! Lua, read the log and inspect loader state without the user having to press keybinds and
//! paste log excerpts back.
//!
//! Enabled from UE4SS-settings.ini:
//!
//! ```ini
//! [MCP]
//! Enabled = 1
//! BindAddress = 127.0.0.1
//! Port = 8787
//! ```
//!
//! Binding defaults to loopback deliberately: this executes arbitrary Lua inside the game
//! process, so it must not be reachable from off-machine. It is a development tool and should
//! stay disabled for normal play.

pub mod ffi;
pub mod tools;

use ffi::{from_utf16_ptr, Host, McpHost};
use std::sync::Mutex;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;

/// Configuration handed over from the C++ settings parser.
#[repr(C)]
pub struct McpConfig {
    /// Null-terminated UTF-16, e.g. "127.0.0.1".
    pub bind_address: *const u16,
    pub port: u16,
}

struct ServerState {
    runtime: Runtime,
    shutdown: Option<oneshot::Sender<()>>,
}

static SERVER: Mutex<Option<ServerState>> = Mutex::new(None);

/// Start the MCP server. Returns false if it is already running or failed to bind.
///
/// # Safety
/// `config` and `host` must be valid for the duration of the call, and every function pointer
/// in `host` must remain valid until [`mcp_stop`] returns.
#[no_mangle]
pub unsafe extern "C" fn mcp_start(config: &McpConfig, host: &McpHost) -> bool {
    let host = Host(*host);

    let mut guard = match SERVER.lock() {
        Ok(g) => g,
        Err(_) => {
            host.log("[MCP] internal lock poisoned; not starting");
            return false;
        }
    };
    if guard.is_some() {
        host.log("[MCP] already running");
        return false;
    }

    let bind = {
        let addr = from_utf16_ptr(config.bind_address);
        let addr = if addr.is_empty() {
            "127.0.0.1".to_string()
        } else {
            addr
        };
        format!("{}:{}", addr, config.port)
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        // Two workers is plenty: tool calls are dominated by blocking on the game thread,
        // not by CPU. Keeping the pool small keeps the footprint inside the game modest.
        .worker_threads(2)
        .enable_all()
        .thread_name("ue4ss-mcp")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            host.log(&format!("[MCP] failed to create runtime: {e}"));
            return false;
        }
    };

    let (tx, rx) = oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let bind_for_task = bind.clone();
    runtime.spawn(async move {
        match tools::serve(bind_for_task, host, rx).await {
            Ok(()) => {}
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
                return;
            }
        }
        let _ = ready_tx.send(Ok(()));
    });

    // Wait briefly for the bind to succeed or fail, so the log says which rather than
    // silently doing nothing when the port is taken.
    match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Err(e)) => {
            host.log(&format!("[MCP] failed to start on {bind}: {e}"));
            return false;
        }
        // Ok(Ok(())) means the server finished immediately, which only happens on shutdown.
        // A timeout is the normal case: the accept loop is running.
        _ => {}
    }

    host.log(&format!("[MCP] listening on http://{bind}/mcp"));
    *guard = Some(ServerState {
        runtime,
        shutdown: Some(tx),
    });
    true
}

/// Stop the server and tear down its runtime. Safe to call when not running.
#[no_mangle]
pub extern "C" fn mcp_stop() {
    let state = match SERVER.lock() {
        Ok(mut g) => g.take(),
        Err(_) => return,
    };
    if let Some(mut state) = state {
        if let Some(tx) = state.shutdown.take() {
            let _ = tx.send(());
        }
        // Give in-flight requests a moment, then drop the runtime.
        state
            .runtime
            .shutdown_timeout(std::time::Duration::from_secs(2));
    }
}

/// Whether the server is currently running.
#[no_mangle]
pub extern "C" fn mcp_is_running() -> bool {
    SERVER.lock().map(|g| g.is_some()).unwrap_or(false)
}

