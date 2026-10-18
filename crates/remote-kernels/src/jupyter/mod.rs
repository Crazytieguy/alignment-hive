//! Client for a machine's Jupyter Server, reached through whatever
//! [`crate::runtime::JupyterEndpoint`] the runtime built: REST for kernel
//! lifecycle ([`rest`]), one WebSocket per kernel for execution ([`ws`]), and
//! the message types plus output assembly they share ([`messages`]).

pub mod messages;
pub mod rest;
pub mod ws;
