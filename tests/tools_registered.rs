//! Guards the tool surface.
//!
//! `#[tool_router]` / `#[tool_handler]` wire the tools up via proc macros, so a mistake there
//! fails silently: the server starts, answers `tools/list` with an empty array, and every
//! call 404s. That is indistinguishable from "the game isn't running" when debugging from the
//! client side, so assert it here instead.

use std::ffi::c_void;

use mcp_bind::ffi::{Host, McpHost, McpString};

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
extern "C" fn on_tool_call(_ctx: *mut c_void, _t: *const u16, _a: *const u16, _ok: bool) {}
extern "C" fn free_string(_ctx: *mut c_void, _s: *mut McpString) {}

fn test_host() -> Host {
    Host(McpHost {
        ctx: std::ptr::null_mut(),
        log,
        lua_eval,
        game_status,
        log_tail,
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
    assert!(names.contains(&"game_status"), "missing game_status: {names:?}");
    assert!(names.contains(&"log_tail"), "missing log_tail: {names:?}");
    assert_eq!(names.len(), 3, "unexpected tool set: {names:?}");

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
