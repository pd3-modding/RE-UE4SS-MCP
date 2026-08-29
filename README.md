# mcp_bind

An [MCP](https://modelcontextprotocol.io) server hosted **inside** a running Unreal Engine game,
built as a static library that [UE4SS](https://github.com/UE4SS-RE/RE-UE4SS) links in.

It lets an MCP client (Claude Code, or anything else that speaks the protocol) evaluate Lua on
the game thread, read the loader's state, and tail `UE4SS.log` — without the usual
edit → rebuild → relaunch → press-a-keybind → paste-the-log loop.

```
MCP client ──HTTP /mcp──▶ mcp_bind (Rust: rmcp + axum + tokio)
                              │  extern "C" McpHost callbacks
                              ▼
                          UE4SS (C++): marshals to the game thread, runs Lua,
                          logs each call, shows recent calls in the GUI
```

## Tools

| Tool | Purpose |
| --- | --- |
| `lua_eval(code)` | Execute Lua on the game thread; returns captured output and the returned value. `FindAllOf`, property reads, `UFunction` calls. |
| `game_status()` | Loader and game state as JSON: version, whether a world is loaded, which mods are running. |
| `log_tail(lines, filter)` | Tail `UE4SS.log`, optionally filtered by substring. |

## Security

**This executes arbitrary code inside the game process.** It is a development tool.

* Binding defaults to `127.0.0.1` so it is not reachable off-machine.
* It is off unless explicitly enabled in the ini.
* There is no authentication — loopback-only is the whole security model. Do not bind it to
  a routable address, and do not leave it enabled during online play.

## Building

The crate is a normal Cargo project and builds standalone:

```sh
cargo build
cargo test
```

`crate-type = ["staticlib", "lib"]` — `staticlib` is what C++ links against, `lib` is what the
tests use.

Inside a UE4SS build it is imported by Corrosion, gated behind `UE4SS_ENABLE_MCP` (off by
default: the tokio/axum dependency tree adds noticeably to build time).

```cmake
corrosion_import_crate(MANIFEST_PATH ".../mcp_bind/Cargo.toml")
target_link_libraries(<your-target> PRIVATE mcp_bind)
target_include_directories(<your-target> PRIVATE ".../mcp_bind/include")
```

## Integrating

`include/mcp_bind.h` is the contract; `src/ffi.rs` is the other half of it. **Change both in the
same commit.** The header documents the two rules that matter:

* **String ownership.** C++ allocates the `McpString` out-params; Rust copies the contents and
  calls `free_string`. Strings going the other way are null-terminated and valid only for the
  duration of the call.
* **Threading.** Every callback runs on a tokio worker, *never* the game thread — anything
  touching game state must marshal there itself. `lua_eval` is expected to block until the game
  thread has run the code, so **the host must apply its own timeout**: if the game thread is
  wedged mid-map-load, returning an error beats hanging the HTTP request forever.

Then:

```cpp
McpHost host{ /* ctx + six callbacks */ };
McpConfig config{ L"127.0.0.1", 8787 };
mcp_start(&config, &host);
// ...
mcp_stop();
```

## Tests

`tests/tools_registered.rs` asserts the tool surface exists. This is not busywork: `#[tool_router]`
and `#[tool_handler]` wire tools up through proc macros, and a mistake there fails *silently* —
the server starts, `tools/list` returns an empty array, and every call 404s. From the client that
is indistinguishable from "the game isn't running", so it is worth catching at build time.

## License

MIT.
