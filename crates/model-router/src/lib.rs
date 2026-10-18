//! A loopback gateway Claude Code uses as its `ANTHROPIC_BASE_URL`. Requests
//! for a routed model ID go to GPT, Grok, or OpenAI-compatible backends
//! through `CLIProxyAPI` (normally a child the router supervises); everything
//! else is forwarded to Anthropic. The request path starts in [`proxy`];
//! [`supervisor`] owns the child, [`service`] the OS service, and [`doctor`]
//! and [`settings_edit`] the Claude Code side of setup.

#![warn(clippy::pedantic)]

pub mod acquire;
mod capture;
mod claude_settings;
mod client_window;
pub mod config;
mod context_check;
pub mod discovery;
pub mod doctor;
mod headers;
mod identity;
mod overflow;
mod prompt_cache;
pub mod proxy;
mod routing;
pub mod service;
pub mod settings_edit;
mod sse;
pub mod state;
mod stub;
pub mod supervisor;
mod tool_schema;
mod usage;
pub mod verify;
mod websearch;
