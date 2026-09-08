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
pub mod state;
mod stub;
pub mod supervisor;
mod usage;
pub mod verify;
mod websearch;
