//! The canned reply `mode = "stub"` serves on the GPT branch: a protocol
//! smoke test that needs no `CLIProxyAPI`.

use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::json;

#[must_use]
pub fn response(model: &str, streaming: bool) -> (HeaderMap, Vec<Bytes>) {
    let message = json!({
        "id": "msg_model_router_stub",
        "type": "message",
        "role": "assistant",
        "content": [{"type":"text","text":"model-router stub response"}],
        "model": model,
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens":1,"output_tokens":4}
    });
    crate::proxy::frame_message(&message, streaming)
}
