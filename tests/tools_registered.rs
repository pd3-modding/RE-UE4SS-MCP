//! Guards the tool surface.
//!
//! `#[tool_router]` / `#[tool_handler]` wire the tools up via proc macros, so a mistake there
//! fails silently: the server starts, answers `tools/list` with an empty array, and every
//! call 404s. That is indistinguishable from "the game isn't running" when debugging from the
//! client side, so assert it here instead.

use std::ffi::c_void;
use std::sync::Mutex;

use mcp_bind::ffi::{from_utf16_ptr, Host, McpHost, McpString};

// Minimal no-op host. These are never invoked by the assertions below -- constructing the
// server only needs the vtable to exist.
extern "C" fn log(_ctx: *mut c_void, _msg: *const u16) {}
extern "C" fn lua_eval(_ctx: *mut c_void, _code: *const u16, _out: *mut McpString) -> bool {
    false
}
extern "C" fn game_status(_ctx: *mut c_void, _out: *mut McpString) -> bool {
    false
}
extern "C" fn log_tail(
    _ctx: *mut c_void,
    _lines: u32,
    _filter: *const u16,
    _out: *mut McpString,
) -> bool {
    false
}
extern "C" fn reload_mods(_ctx: *mut c_void, _out: *mut McpString) -> bool {
    false
}
extern "C" fn execute_console_command(
    _ctx: *mut c_void,
    _command: *const u16,
    _out: *mut McpString,
) -> bool {
    false
}
extern "C" fn on_tool_call(_ctx: *mut c_void, _t: *const u16, _a: *const u16, _ok: bool) {}
extern "C" fn free_string(_ctx: *mut c_void, _s: *mut McpString) {}

fn test_host() -> Host {
    Host(McpHost {
        ctx: std::ptr::null_mut(),
        log,
        lua_eval,
        game_status,
        log_tail,
        reload_mods,
        execute_console_command,
        on_tool_call,
        free_string,
    })
}

#[test]
fn every_tool_is_registered_and_described() {
    let server = mcp_bind::tools::Pd3Server::new(test_host());
    let tools = server.tool_router.list_all();

    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"lua_eval"), "missing lua_eval: {names:?}");
    assert!(
        names.contains(&"game_status"),
        "missing game_status: {names:?}"
    );
    assert!(names.contains(&"log_tail"), "missing log_tail: {names:?}");
    assert!(
        names.contains(&"reload_mods"),
        "missing reload_mods: {names:?}"
    );
    assert!(
        names.contains(&"execute_console_command"),
        "missing execute_console_command: {names:?}"
    );
    assert_eq!(names.len(), 5, "unexpected tool set: {names:?}");

    // A tool with no description is close to useless to a model, so treat it as a failure.
    for tool in &tools {
        let description = tool.description.as_deref().unwrap_or("");
        assert!(
            description.len() > 20,
            "tool '{}' has a missing or too-short description",
            tool.name
        );
    }
}

/// A session must log exactly one connect and one disconnect, no matter how many times rmcp
/// clones the handler internally. A bare `Drop` on `Pd3Server` would emit a disconnect per
/// clone, which is why the guard sits behind an `Arc`.
#[test]
fn session_logs_connect_and_disconnect_exactly_once() {
    // ctx points at this; the callback pushes every logged line into it.
    let captured: Mutex<Vec<String>> = Mutex::new(Vec::new());

    extern "C" fn capture(ctx: *mut c_void, msg: *const u16) {
        let sink = unsafe { &*(ctx as *const Mutex<Vec<String>>) };
        sink.lock().unwrap().push(unsafe { from_utf16_ptr(msg) });
    }

    let host = Host(McpHost {
        ctx: &captured as *const _ as *mut c_void,
        log: capture,
        lua_eval,
        game_status,
        log_tail,
        reload_mods,
        execute_console_command,
        on_tool_call,
        free_string,
    });

    // Never hold this lock across a drop: the log callback takes the same mutex, and a
    // std::sync::Mutex is not reentrant, so doing so deadlocks rather than fails.
    let count = |needle: &str| {
        captured
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(needle))
            .count()
    };

    {
        let server = mcp_bind::tools::Pd3Server::new(host);
        let clones = vec![server.clone(), server.clone(), server.clone()];

        assert_eq!(
            count("client connected"),
            1,
            "expected exactly one connect line"
        );
        assert_eq!(
            count("client disconnected"),
            0,
            "disconnected while the session was still alive"
        );

        drop(clones);
        assert_eq!(
            count("client disconnected"),
            0,
            "dropping a clone reported a disconnect while the original was still alive"
        );
    }

    assert_eq!(
        count("client disconnected"),
        1,
        "expected exactly one disconnect line after the session dropped"
    );
}

#[test]
fn server_info_advertises_tools() {
    use rmcp::ServerHandler;

    let server = mcp_bind::tools::Pd3Server::new(test_host());
    let info = server.get_info();

    assert!(
        info.capabilities.tools.is_some(),
        "server does not advertise the tools capability, so no client will call one"
    );
    assert!(info.instructions.is_some(), "server has no instructions");
    assert_eq!(info.server_info.name, "ue4ss-pd3");
}
