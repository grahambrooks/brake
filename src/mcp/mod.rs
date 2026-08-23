//! The MCP server: the same ruleset, consulted at edit time.
//!
//! `brake check` runs at the moment of commit, which is the last moment a
//! change can be stopped and the worst moment to learn about it. An agent
//! editing an OpenAPI file has the intent in hand and has not written the
//! change yet. This is a delivery channel for the existing ruleset, not a
//! second product — see `design/04-mcp-interface.md`.
//!
//! # Shape
//!
//! [`handlers`] holds the logic and is **synchronous and transport-free**:
//! every tool is a plain function from arguments to a JSON value, calling the
//! same library functions the CLI calls. [`server`] is the `rmcp` adapter, and
//! is the only part that knows about async or about MCP at all.
//!
//! That split is deliberate. It means the tools are tested without spawning a
//! process or driving a protocol, and it means there is one implementation of
//! every verdict rather than a second one to keep honest.
//!
//! # Trust posture
//!
//! Read `design/04-mcp-interface.md` §5 before changing anything here. The
//! load-bearing exclusion: **`[contract.generated]` is never honoured over
//! MCP**. Drift runs a command out of a config file, and an agent that can
//! write `brake.toml` — which any agent editing a repository can — would then
//! have arbitrary command execution through a tool whose stated purpose is
//! reading files. [`handlers::Options`] has no drift switch to set, and
//! `tests/mcp.rs` asserts a declared generator stays unexecuted.

pub mod handlers;

#[cfg(feature = "mcp")]
pub mod server;

#[cfg(feature = "mcp")]
pub use server::serve_stdio;
