use axum::http::{HeaderMap, HeaderValue, header};
use bytes::Bytes;
use serde_json::json;

#[must_use]
pub fn response(model: &str, streaming: bool) -> (HeaderMap, Vec<Bytes>) {
    if streaming {
        streaming_response(model)
    } else {
        json_response(model)
    }
}

fn streaming_response(model: &str) -> (HeaderMap, Vec<Bytes>) {
    let events = [
        (
            "message_start",
            json!({"type":"message_start","message":{"id":"msg_model_router_stub","type":"message","role":"assistant","content":[],"model":model,"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":0}}}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"model-router stub response"}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":4}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ];
    let chunks = events
        .into_iter()
        .map(|(event, data)| Bytes::from(format!("event: {event}\ndata: {data}\n\n")))
        .collect();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    (headers, chunks)
}

fn json_response(model: &str) -> (HeaderMap, Vec<Bytes>) {
    let body = json!({
        "id": "msg_model_router_stub",
        "type": "message",
        "role": "assistant",
        "content": [{"type":"text","text":"model-router stub response"}],
        "model": model,
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens":1,"output_tokens":4}
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    (headers, vec![Bytes::from(body.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_streaming_response_is_a_valid_anthropic_message() {
        let (headers, chunks) = response("gpt-test", false);
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        let value: serde_json::Value = serde_json::from_slice(&chunks.concat()).unwrap();
        assert_eq!(value["type"], "message");
        assert_eq!(value["model"], "gpt-test");
        assert_eq!(value["stop_reason"], "end_turn");
    }

    #[test]
    fn streaming_response_has_required_event_sequence() {
        let (_, chunks) = response("gpt-test", true);
        let body = String::from_utf8(chunks.concat()).unwrap();
        let positions = [
            "event: message_start",
            "event: content_block_start",
            "event: content_block_delta",
            "event: message_delta",
            "event: message_stop",
        ]
        .map(|event| body.find(event).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
