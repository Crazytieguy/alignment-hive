use serde::{Deserialize, Serialize};

use crate::images::ImageCollector;

/// Jupyter WebSocket message envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JupyterMessage {
    pub channel: String,
    pub header: Header,
    #[serde(default)]
    pub parent_header: serde_json::Value,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub content: serde_json::Value,
    #[serde(default)]
    pub buffers: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub msg_id: String,
    pub msg_type: String,
    #[serde(default = "default_username")]
    pub username: String,
    pub session: String,
    #[serde(default)]
    pub date: String,
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_username() -> String {
    "remote-kernels".to_string()
}

fn default_version() -> String {
    "5.3".to_string()
}

impl JupyterMessage {
    /// Create an `execute_request` message.
    pub fn execute_request(session_id: &str, code: &str) -> Self {
        Self {
            channel: "shell".to_string(),
            header: Header {
                msg_id: uuid::Uuid::new_v4().to_string(),
                msg_type: "execute_request".to_string(),
                username: "remote-kernels".to_string(),
                session: session_id.to_string(),
                date: String::new(),
                version: "5.3".to_string(),
            },
            parent_header: serde_json::Value::Object(serde_json::Map::new()),
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            content: serde_json::json!({
                "code": code,
                "silent": false,
                "store_history": true,
                "user_expressions": {},
                "allow_stdin": false,
                "stop_on_error": true
            }),
            buffers: vec![],
        }
    }
}

/// Parsed output from a kernel execution.
#[derive(Debug, Clone, Default)]
pub struct ExecutionOutput {
    /// Outputs in the order the kernel produced them.
    pub outputs: Vec<Output>,
    pub error: Option<ErrorInfo>,
    pub status: ExecutionStatus,
    /// A `clear_output(wait=True)` waiting for the next output to clear.
    clear_pending: bool,
}

/// One output of an execution, as nbformat records it.
#[derive(Debug, Clone)]
pub enum Output {
    /// Consecutive text on one stream (`stdout` or `stderr`), merged.
    Stream {
        stderr: bool,
        text: String,
    },
    Result(DisplayOutput),
    Display(DisplayOutput),
}

/// A rich output (`execute_result` or `display_data`) as the kernel sent it:
/// the whole mime bundle, so the notebook keeps every representation.
#[derive(Debug, Clone, Default)]
pub struct DisplayOutput {
    pub data: serde_json::Map<String, serde_json::Value>,
    pub metadata: serde_json::Value,
}

/// Image mimetypes returned to the model, in order of preference when a
/// bundle carries several.
const RETURNED_IMAGE_TYPES: [&str; 2] = ["image/png", "image/jpeg"];

impl DisplayOutput {
    /// A bundle holding only `text/plain`.
    pub fn text_only(text: &str) -> Self {
        let mut data = serde_json::Map::new();
        data.insert("text/plain".to_string(), text.into());
        Self {
            data,
            metadata: serde_json::json!({}),
        }
    }

    /// The `text/plain` representation (nbformat allows a string or a list
    /// of line strings).
    pub fn text(&self) -> Option<String> {
        mime_string(self.data.get("text/plain")?)
    }

    /// The preferred returnable image: its mimetype and base64 payload.
    pub fn image(&self) -> Option<(&'static str, String)> {
        RETURNED_IMAGE_TYPES
            .iter()
            .find_map(|&mime| Some((mime, mime_string(self.data.get(mime)?)?)))
    }

    /// The first image mimetype that is not returned to the model (SVG, GIF…).
    fn unreturned_image_type(&self) -> Option<&str> {
        self.data
            .keys()
            .map(String::as_str)
            .find(|mime| mime.starts_with("image/") && !RETURNED_IMAGE_TYPES.contains(mime))
    }

    /// The text that stands in for this output in a tool reply: an image
    /// marker when the image is returned, otherwise `text/plain`.
    fn render(&self, images: &mut ImageCollector) -> Option<String> {
        if let Some((mime, data)) = self.image() {
            return Some(images.add(mime, &data));
        }
        let note = self.unreturned_image_type().map(|mime| {
            format!("[{mime} output not shown: only PNG and JPEG images are returned]")
        });
        match (self.text(), note) {
            (Some(text), Some(note)) => Some(format!("{text}\n{note}")),
            (text, note) => text.or(note),
        }
    }
}

fn mime_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(lines) => lines.iter().map(|line| line.as_str()).collect(),
        _ => None,
    }
}

fn display_output(content: &serde_json::Value) -> Option<DisplayOutput> {
    let data = content["data"]
        .as_object()
        .filter(|data| !data.is_empty())?;
    Some(DisplayOutput {
        data: data.clone(),
        metadata: content
            .get("metadata")
            .filter(|metadata| metadata.is_object())
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExecutionStatus {
    #[default]
    Running,
    Complete,
    Errored,
}

#[derive(Debug, Clone)]
pub struct ErrorInfo {
    pub ename: String,
    pub evalue: String,
    pub traceback: Vec<String>,
}

impl ExecutionOutput {
    /// Create an error output with a message.
    pub fn error(msg: &str) -> Self {
        let mut output = Self {
            status: ExecutionStatus::Errored,
            ..Default::default()
        };
        output.push_stream(true, msg);
        output
    }

    /// Append stream text, merging with the previous output when it is the
    /// same stream (as Jupyter does).
    pub fn push_stream(&mut self, stderr: bool, text: &str) {
        if text.is_empty() {
            return;
        }
        self.apply_pending_clear();
        if let Some(Output::Stream {
            stderr: last_stderr,
            text: last,
        }) = self.outputs.last_mut()
            && *last_stderr == stderr
        {
            last.push_str(text);
        } else {
            self.outputs.push(Output::Stream {
                stderr,
                text: text.to_string(),
            });
        }
    }

    /// Append a rich output.
    fn push_rich(&mut self, output: Output) {
        self.apply_pending_clear();
        self.outputs.push(output);
    }

    /// `clear_output(wait=True)` clears when the next output arrives, so a
    /// redraw loop never shows an empty cell (as in Jupyter).
    fn apply_pending_clear(&mut self) {
        if std::mem::take(&mut self.clear_pending) {
            self.outputs.clear();
        }
    }

    /// All text written to one stream.
    pub fn stream_text(&self, stderr: bool) -> String {
        self.outputs
            .iter()
            .filter_map(|output| match output {
                Output::Stream {
                    stderr: is_stderr,
                    text,
                } if *is_stderr == stderr => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Format the output for a tool reply, in the order it was produced.
    /// Returned images go to `images`, which decodes them to validate (so call
    /// this off the async runtime); the text carries a numbered marker in each
    /// one's place.
    pub fn render(&self, images: &mut ImageCollector) -> String {
        let mut parts = Vec::new();

        // Stdout that follows a stderr block is labelled, so it isn't read
        // as more stderr.
        let mut after_stderr = false;
        for output in &self.outputs {
            let part = match output {
                Output::Stream { stderr: true, text } => Some(format!("[stderr]\n{text}")),
                Output::Stream {
                    stderr: false,
                    text,
                } if after_stderr => Some(format!("[stdout]\n{text}")),
                Output::Stream {
                    stderr: false,
                    text,
                } => Some(text.clone()),
                Output::Result(data) | Output::Display(data) => data.render(images),
            };
            if let Some(part) = part {
                after_stderr = matches!(output, Output::Stream { stderr: true, .. });
                parts.push(part);
            }
        }

        if let Some(ref err) = self.error {
            let tb = err.traceback.join("\n");
            parts.push(format!("{}: {}\n{tb}", err.ename, err.evalue));
        }

        if parts.is_empty() {
            return "(no output)".to_string();
        }
        // One line break between parts; stream text usually brings its own.
        let mut text = String::new();
        for part in parts {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&part);
        }
        text
    }

    /// Process an incoming iopub message, updating the output state.
    pub fn process_iopub(&mut self, msg: &JupyterMessage) {
        let msg_type = msg.header.msg_type.as_str();
        match msg_type {
            "stream" => {
                let stderr = msg.content["name"].as_str() == Some("stderr");
                let text = msg.content["text"].as_str().unwrap_or("");
                self.push_stream(stderr, text);
            }
            "execute_result" => {
                if let Some(output) = display_output(&msg.content) {
                    self.push_rich(Output::Result(output));
                }
            }
            "display_data" => {
                if let Some(output) = display_output(&msg.content) {
                    self.push_rich(Output::Display(output));
                }
            }
            // Live-plot loops redraw with this; without it every frame would
            // pile up (and the oldest would take the reply's image slots).
            "clear_output" => {
                if msg.content["wait"].as_bool() == Some(true) {
                    self.clear_pending = true;
                } else {
                    self.clear_pending = false;
                    self.outputs.clear();
                }
            }
            "error" => {
                // The error is the next output, so a deferred clear lands first.
                self.apply_pending_clear();
                let ename = msg.content["ename"].as_str().unwrap_or("Error").to_string();
                let evalue = msg.content["evalue"].as_str().unwrap_or("").to_string();
                let traceback = msg.content["traceback"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                self.error = Some(ErrorInfo {
                    ename,
                    evalue,
                    traceback,
                });
                self.status = ExecutionStatus::Errored;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn iopub(msg_type: &str, content: serde_json::Value) -> JupyterMessage {
        JupyterMessage {
            channel: "iopub".to_string(),
            header: Header {
                msg_id: String::new(),
                msg_type: msg_type.to_string(),
                username: String::new(),
                session: String::new(),
                date: String::new(),
                version: String::new(),
            },
            parent_header: serde_json::json!({}),
            metadata: serde_json::json!({}),
            content,
            buffers: Vec::new(),
        }
    }

    #[test]
    fn outputs_render_in_the_order_produced() {
        let mut output = ExecutionOutput::default();
        let stream = |name: &str, text: &str| {
            iopub("stream", serde_json::json!({"name": name, "text": text}))
        };
        output.process_iopub(&stream("stdout", "a\n"));
        output.process_iopub(&stream("stdout", "b\n"));
        output.process_iopub(&iopub(
            "display_data",
            serde_json::json!({
                "data": {
                    "text/plain": "<Figure size 640x480 with 1 Axes>",
                    "image/png": PNG_1X1,
                },
                "metadata": {"image/png": {"width": 1}},
            }),
        ));
        output.process_iopub(&stream("stderr", "warn\n"));
        output.process_iopub(&stream("stdout", "c\n"));
        output.process_iopub(&iopub(
            "execute_result",
            serde_json::json!({"data": {"text/plain": ["4", "2"]}, "execution_count": 1}),
        ));

        let mut images = ImageCollector::default();
        let text = output.render(&mut images);
        // The image's `<Figure …>` text is replaced by its marker, in place.
        assert_eq!(
            text,
            "a\nb\n[image 1: 1x1 PNG]\n[stderr]\nwarn\n[stdout]\nc\n42"
        );
        assert_eq!(images.images.len(), 1);
        assert_eq!(images.images[0].mime, "image/png");
        assert_eq!(output.stream_text(false), "a\nb\nc\n");
        // The notebook keeps the whole bundle.
        let Output::Display(display) = &output.outputs[1] else {
            panic!("expected display_data second: {:?}", output.outputs);
        };
        assert_eq!(display.data.len(), 2);
        assert_eq!(display.metadata["image/png"]["width"], 1);
    }

    #[test]
    fn clear_output_drops_earlier_frames() {
        let frame = |n: &str| {
            iopub(
                "display_data",
                serde_json::json!({"data": {"text/plain": n, "image/png": PNG_1X1}}),
            )
        };
        let stdout = |text: &str| {
            iopub(
                "stream",
                serde_json::json!({"name": "stdout", "text": text}),
            )
        };

        // wait=True: each clear lands only when the next output arrives.
        let mut output = ExecutionOutput::default();
        for i in 0..20 {
            output.process_iopub(&iopub("clear_output", serde_json::json!({"wait": true})));
            output.process_iopub(&stdout(&format!("step {i}\n")));
            output.process_iopub(&frame(&i.to_string()));
        }
        let mut images = ImageCollector::default();
        assert_eq!(output.render(&mut images), "step 19\n[image 1: 1x1 PNG]");
        assert_eq!(images.images.len(), 1);
        assert_eq!(images.over_cap, 0);

        // A trailing wait=True clear with nothing after it clears nothing.
        output.process_iopub(&iopub("clear_output", serde_json::json!({"wait": true})));
        assert_eq!(output.outputs.len(), 2);

        // Immediate clear.
        output.process_iopub(&iopub("clear_output", serde_json::json!({"wait": false})));
        assert!(output.outputs.is_empty());
        output.process_iopub(&stdout("done\n"));
        assert_eq!(output.stream_text(false), "done\n");

        // An error raised mid-redraw replaces the deferred-cleared frame.
        let mut output = ExecutionOutput::default();
        output.process_iopub(&frame("0"));
        output.process_iopub(&iopub("clear_output", serde_json::json!({"wait": true})));
        output.process_iopub(&iopub(
            "error",
            serde_json::json!({"ename": "ValueError", "evalue": "bad", "traceback": []}),
        ));
        let mut images = ImageCollector::default();
        assert_eq!(output.render(&mut images), "ValueError: bad\n");
        assert!(images.images.is_empty());
        assert!(output.outputs.is_empty());
        assert_eq!(output.status, ExecutionStatus::Errored);
    }

    #[test]
    fn png_is_preferred_and_unreturned_types_are_noted() {
        let mut both = serde_json::Map::new();
        both.insert("image/jpeg".to_string(), "not used".into());
        both.insert("image/png".to_string(), PNG_1X1.into());
        let both = DisplayOutput {
            data: both,
            metadata: serde_json::json!({}),
        };
        assert_eq!(both.image().unwrap().0, "image/png");

        let mut svg = DisplayOutput::text_only("<Figure>");
        svg.data
            .insert("image/svg+xml".to_string(), "<svg/>".into());
        let mut images = ImageCollector::default();
        assert_eq!(
            svg.render(&mut images).unwrap(),
            "<Figure>\n[image/svg+xml output not shown: only PNG and JPEG images are returned]"
        );
        assert!(images.images.is_empty());
    }
}
