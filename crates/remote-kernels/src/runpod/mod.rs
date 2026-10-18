//! Typed client for the `RunPod` REST v2 API. Provider policy (GPU fallback,
//! the orphan guard, access paths) lives in [`crate::runtime::runpod`].

pub mod client;
pub mod types;
