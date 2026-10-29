//! A loopback gateway Claude Code uses as its `ANTHROPIC_BASE_URL`, or
//! reaches as its `HTTPS_PROXY` (the front in [`mitm`]). Requests
//! for a routed model ID go to GPT, Grok, or OpenAI-compatible backends
//! through `CLIProxyAPI` (normally a child the router supervises); everything
//! else is forwarded to Anthropic. The request path starts in [`proxy`];
//! [`supervisor`] owns the child, [`service`] the OS service, and [`doctor`]
//! and [`settings_edit`] the Claude Code side of setup.

#![warn(clippy::pedantic)]

pub mod acquire;
pub mod auto_mode;
mod cache_marks;
mod capture;
mod claude_settings;
mod client_window;
pub mod config;
mod context_check;
pub mod discovery;
pub mod doctor;
mod exclusions;
mod headers;
mod identity;
mod logins;
pub mod mitm;
pub mod muse;
mod overflow;
mod prompt_cache;
pub mod proxy;
mod reasoning_order;
mod routing;
pub mod service;
mod service_tier;
pub mod settings_edit;
mod sse;
pub mod state;
mod stub;
pub mod supervisor;
mod tool_changes;
mod tool_schema;
mod usage;
pub mod verify;
mod websearch;
