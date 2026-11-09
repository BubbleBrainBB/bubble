//! Bubble's agent core in Rust.
//!
//! It knows nothing about the UI: the app (and later a CLI or a home host)
//! drives it through these types.

pub mod agent;
pub mod auth;
pub mod compact;
pub mod config;
pub mod delegate;
pub mod discover;
pub mod gemini;
pub mod hooks;
pub mod instructions;
pub mod login;
pub mod mcp;
pub mod mcp_oauth;
pub mod memory;
pub mod model;
pub mod prompt;
pub mod provider;
pub mod responses;
pub mod routing;
pub mod skills;
pub mod sse;
pub mod time;
pub mod toolbox;
pub mod tools;
pub mod web;
pub mod workflow;
pub mod worktree;

pub use agent::{AgentEvent, Decision, Turn, run_turn};
pub use config::{Config, Protocol, ProviderConfig, ResolvedModel, bubble_home};
pub use provider::{
    ChatMessage, ChatOutcome, ChatRequest, Image, StreamEvent, ToolCall, ToolResult, ToolSpec, Usage, http_client, stream_chat,
};
pub use model::Level;
pub use toolbox::Toolbox;
pub use tools::{Permission, ToolOutput};

/// Starts `command` in a session of its own, with no controlling
/// terminal: a child (an interactive shell, a command that asks for a
/// password) cannot take the terminal from a full-screen session, nor read
/// what the person types.
pub fn detach(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; nothing else runs in the child.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// [`detach`] for a tokio command.
pub fn detach_async(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        // SAFETY: as in `detach`.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = command;
}
