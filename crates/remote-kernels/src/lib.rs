#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::must_use_candidate)]

/// Install the process-wide rustls crypto provider. kube and tungstenite link
/// rustls with different provider features (aws-lc-rs vs ring), and rustls
/// requires an explicit choice when both are present. Idempotent.
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub mod config;
pub mod descriptions;
pub mod heartbeat;
pub mod jupyter;
pub mod notebook;
pub mod runpod;
pub mod runtime;
pub mod server;
pub mod ssh;
pub mod ssh_exec;
pub mod state;
pub mod sync;
