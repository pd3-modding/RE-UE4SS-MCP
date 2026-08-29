#pragma once

// C++ side of the mcp_bind FFI boundary.
//
// This header is the contract. It lives in the same repository as the Rust that implements
// it so the two cannot drift apart across a submodule bump: if you change `McpHost` here,
// change `src/ffi.rs` in the same commit.
//
// Ownership rule for strings crossing the boundary:
//   * C++ -> Rust (McpString out-params): C++ allocates. Rust copies the contents and then
//     calls `free_string` with the same struct. Rust never frees C++ memory itself, and
//     never holds the buffer past the call.
//   * Rust -> C++ (const McpChar*): null-terminated, owned by Rust, valid only for the
//     duration of the call. Copy it if you need to keep it.
//
// Threading contract, which the C++ side must honour:
//   * Every callback is invoked from a tokio worker thread, NEVER the game thread. Anything
//     that touches game state has to marshal to the game thread itself.
//   * `lua_eval` is expected to BLOCK until the game thread has run the code, because MCP
//     tool calls are request/response. The C++ side must therefore apply its own timeout --
//     if the game thread is wedged (loading a map, or crashed), returning an error beats
//     hanging the HTTP request forever.

#include <cstddef>
#include <cstdint>

// UE4SS uses wchar_t (16-bit on Windows) for File::StringType; Rust uses u16. They are
// layout-compatible there, so alias to wchar_t on Windows to avoid casting at every call.
#ifdef _WIN32
using McpChar = wchar_t;
#else
using McpChar = char16_t;
#endif

extern "C"
{
    // A UTF-16 string allocated by C++ and released via McpHost::free_string.
    struct McpString
    {
        const McpChar* data;
        size_t len;
    };

    // Everything the MCP server needs from UE4SS. Every pointer must stay valid until
    // mcp_stop() has returned.
    struct McpHost
    {
        // Opaque C++ context, handed back with every callback.
        void* ctx;

        // Write a line to UE4SS.log. Used for server lifecycle and for every tool call, so
        // the main log shows MCP activity alongside everything else.
        void (*log)(void* ctx, const McpChar* msg);

        // Run Lua on the game thread and return its captured output.
        // Returns true on success; `out` receives output either way (error text on failure).
        bool (*lua_eval)(void* ctx, const McpChar* code, McpString* out);

        // A JSON blob describing loader/game state (version, mods, whether a world exists...).
        bool (*game_status)(void* ctx, McpString* out);

        // Last `lines` lines of UE4SS.log, optionally filtered by a substring.
        // `filter` may be null.
        bool (*log_tail)(void* ctx, uint32_t lines, const McpChar* filter, McpString* out);

        // Record a tool invocation so the GUI can show recent calls.
        void (*on_tool_call)(void* ctx, const McpChar* tool, const McpChar* args_json, bool ok);

        // Release a string previously produced by one of the callbacks above.
        void (*free_string)(void* ctx, McpString* s);
    };

    // Configuration, normally parsed out of UE4SS-settings.ini [MCP].
    struct McpConfig
    {
        // Null-terminated, e.g. L"127.0.0.1". Empty/null defaults to loopback.
        const McpChar* bind_address;
        uint16_t port;
    };

    // Start the server. Returns false if already running, or if the bind failed (the reason
    // is written through McpHost::log). Blocks for up to ~3s waiting for the bind result so
    // that a taken port is reported rather than silently doing nothing.
    bool mcp_start(const McpConfig* config, const McpHost* host);

    // Stop the server and tear down its runtime. Safe to call when not running.
    void mcp_stop();

    bool mcp_is_running();
}
