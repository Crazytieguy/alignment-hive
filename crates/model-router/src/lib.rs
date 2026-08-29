#![warn(clippy::pedantic)]

pub mod acquire;
pub mod capture;
mod claude_settings;
pub mod client_window;
pub mod config;
mod context_check;
pub mod discovery;
pub mod doctor;
pub mod headers;
pub mod identity;
mod overflow;
pub mod prompt_cache;
pub mod proxy;
pub mod routing;
pub mod service;
pub mod state;
pub mod stub;
pub mod supervisor;
mod usage;
pub mod verify;
pub mod websearch;
