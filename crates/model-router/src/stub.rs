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
    if streaming {
        (
            crate::proxy::sse_headers(),
            crate::websearch::message_to_sse(&message),
        )
    } else {
        (
            crate::proxy::json_headers(),
            vec![Bytes::from(message.to_string())],
        )
    }
}
