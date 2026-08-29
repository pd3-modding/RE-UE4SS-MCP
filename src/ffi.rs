//! The C++ <-> Rust boundary.
//!
//! Follows the same shape as `patternsleuth_bind`: a `staticlib` exposing `#[no_mangle]
//! extern "C"` entry points, with C++ passing function pointers in for anything Rust needs
//! from the host.
//!
//! Ownership rule for strings crossing the boundary:
//!   * Rust -> C++ : UTF-16, null-terminated, valid only for the duration of the call.
//!   * C++ -> Rust : an `McpString` whose buffer C++ allocated. Rust copies it and then calls
//!     `free_string` on the same struct. Rust never frees C++ memory itself and never assumes
//!     the buffer outlives the call.
//!
//! Threading contract, which the C++ side must honour:
//!   * Every callback here is invoked from a tokio worker thread, never the game thread.
//!   * `lua_eval` is expected to BLOCK until the game thread has run the code, because MCP
//!     tool calls are request/response. The C++ side must therefore apply its own timeout --
//!     if the game thread is wedged (loading a map, or crashed), returning an error beats
//!     hanging the HTTP request forever.

use std::ffi::c_void;

/// A UTF-16 string allocated by C++ and freed via [`McpHost::free_string`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct McpString {
    pub data: *const u16,
    pub len: usize,
}

impl McpString {
    pub const fn empty() -> Self {
        Self {
            data: std::ptr::null(),
            len: 0,
        }
    }

    /// Copy into an owned Rust `String`. Returns empty for a null/zero-length buffer.
    ///
    /// # Safety
    /// `data` must point to `len` valid UTF-16 code units, or be null.
    pub unsafe fn to_string_lossy(&self) -> String {
        if self.data.is_null() || self.len == 0 {
            return String::new();
        }
        let slice = std::slice::from_raw_parts(self.data, self.len);
        String::from_utf16_lossy(slice)
    }
}

/// Everything the MCP server needs from UE4SS. C++ fills this in and passes it to
/// [`mcp_start`]; every pointer must stay valid until [`mcp_stop`] returns.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct McpHost {
    /// Opaque C++ context, handed back with every callback.
    pub ctx: *mut c_void,

    /// Write a line to UE4SS.log. Used for server lifecycle and for every tool call, so the
    /// main log shows MCP activity alongside everything else.
    pub log: extern "C" fn(ctx: *mut c_void, msg: *const u16),

    /// Run Lua on the game thread and return its captured output.
    /// Returns true on success; `out` receives output either way (error text on failure).
    pub lua_eval: extern "C" fn(ctx: *mut c_void, code: *const u16, out: *mut McpString) -> bool,

    /// A JSON blob describing loader/game state (engine version, mods, whether a world
    /// exists, whether ProcessEvent resolved, ...).
    pub game_status: extern "C" fn(ctx: *mut c_void, out: *mut McpString) -> bool,

    /// Last `lines` lines of UE4SS.log, optionally filtered by a substring.
    pub log_tail: extern "C" fn(
        ctx: *mut c_void,
        lines: u32,
        filter: *const u16,
        out: *mut McpString,
    ) -> bool,

    /// Reinstall every mod, picking up edited files from disk. Blocks until the reload has
    /// finished, so a successful return means the mods are back up.
    pub reload_mods: extern "C" fn(ctx: *mut c_void, out: *mut McpString) -> bool,

    /// Record a tool invocation so the Lua Debugger tab can show recent calls.
    pub on_tool_call:
        extern "C" fn(ctx: *mut c_void, tool: *const u16, args_json: *const u16, ok: bool),

    /// Release a string previously produced by one of the callbacks above.
    pub free_string: extern "C" fn(ctx: *mut c_void, s: *mut McpString),
}

// SAFETY: the C++ side guarantees every callback is thread-safe and that `ctx` outlives the
// server. This is the same contract patternsleuth_bind relies on for its logging callback.
unsafe impl Send for McpHost {}
unsafe impl Sync for McpHost {}

/// Safe wrapper so tool handlers are not littered with `unsafe` and manual frees.
#[derive(Clone, Copy)]
pub struct Host(pub McpHost);

impl Host {
    pub fn log(&self, msg: &str) {
        let wide = to_utf16(msg);
        (self.0.log)(self.0.ctx, wide.as_ptr());
    }

    /// Consume an out-parameter the host filled in, always freeing it.
    fn take(&self, mut s: McpString) -> String {
        let text = unsafe { s.to_string_lossy() };
        (self.0.free_string)(self.0.ctx, &mut s as *mut McpString);
        text
    }

    pub fn lua_eval(&self, code: &str) -> Result<String, String> {
        let wide = to_utf16(code);
        let mut out = McpString::empty();
        let ok = (self.0.lua_eval)(self.0.ctx, wide.as_ptr(), &mut out as *mut McpString);
        let text = self.take(out);
        if ok {
            Ok(text)
        } else {
            Err(text)
        }
    }

    pub fn game_status(&self) -> Result<String, String> {
        let mut out = McpString::empty();
        let ok = (self.0.game_status)(self.0.ctx, &mut out as *mut McpString);
        let text = self.take(out);
        if ok {
            Ok(text)
        } else {
            Err(text)
        }
    }

    pub fn log_tail(&self, lines: u32, filter: Option<&str>) -> Result<String, String> {
        let filter_wide = filter.map(to_utf16);
        let filter_ptr = filter_wide
            .as_ref()
            .map(|v| v.as_ptr())
            .unwrap_or(std::ptr::null());
        let mut out = McpString::empty();
        let ok = (self.0.log_tail)(self.0.ctx, lines, filter_ptr, &mut out as *mut McpString);
        let text = self.take(out);
        if ok {
            Ok(text)
        } else {
            Err(text)
        }
    }

    pub fn reload_mods(&self) -> Result<String, String> {
        let mut out = McpString::empty();
        let ok = (self.0.reload_mods)(self.0.ctx, &mut out as *mut McpString);
        let text = self.take(out);
        if ok {
            Ok(text)
        } else {
            Err(text)
        }
    }

    pub fn on_tool_call(&self, tool: &str, args_json: &str, ok: bool) {
        let t = to_utf16(tool);
        let a = to_utf16(args_json);
        (self.0.on_tool_call)(self.0.ctx, t.as_ptr(), a.as_ptr(), ok);
    }
}

/// Null-terminated UTF-16, which is what UE4SS's `File::StringType` expects on Windows.
pub fn to_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read a null-terminated UTF-16 string from C++.
///
/// # Safety
/// `ptr` must be null or point to a null-terminated UTF-16 string.
pub unsafe fn from_utf16_ptr(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
        // Defensive cap: a missing terminator must not walk the whole address space.
        if len > 1 << 22 {
            break;
        }
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}
