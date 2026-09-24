//! Image outputs (plots, `IPython.display.Image`) returned to the model as MCP
//! image content, bounded so a plotting loop cannot flood the context.

use std::io::Cursor;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::{ImageFormat, ImageReader, Limits};

/// Images returned per tool reply; the rest stay in the notebook.
pub const MAX_IMAGES_PER_REPLY: usize = 8;
/// Decoder limits: a hostile or runaway image is refused, not decoded.
const DECODE_MAX_EDGE: u32 = 20_000;
const DECODE_MAX_ALLOC: u64 = 512 * 1024 * 1024;

/// An image ready to return: its mimetype and base64 payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnedImage {
    pub mime: &'static str,
    pub base64: String,
}

/// The images of one tool reply, in the order their markers appear in its text.
#[derive(Debug, Default)]
pub struct ImageCollector {
    pub images: Vec<ReturnedImage>,
    /// Images left out because the reply already held `MAX_IMAGES_PER_REPLY`.
    pub over_cap: usize,
}

impl ImageCollector {
    /// Validate one image output and return the marker that stands in for it
    /// in the reply text.
    pub fn add(&mut self, mime: &str, base64: &str) -> String {
        if self.images.len() >= MAX_IMAGES_PER_REPLY {
            self.over_cap += 1;
            return format!("[image not shown: limit of {MAX_IMAGES_PER_REPLY} per reply]");
        }
        // The image crate is not expected to panic on bad input, but a panic
        // here would take the whole reply with it.
        let prepared = std::panic::catch_unwind(|| prepare(mime, base64))
            .unwrap_or_else(|_| Err("could not be decoded".to_string()));
        match prepared {
            Ok((image, description)) => {
                self.images.push(image);
                format!("[image {}: {description}]", self.images.len())
            }
            Err(reason) => format!("[image not shown: {reason}]"),
        }
    }

    /// The reply footer for images left out by the cap.
    pub fn footer(&self) -> Option<String> {
        (self.over_cap > 0).then(|| {
            format!(
                "{} more image(s) not shown (limit {MAX_IMAGES_PER_REPLY} per reply). They are saved in the kernel's local notebook; display one in a later execute() to see it.",
                self.over_cap
            )
        })
    }
}

/// Decode the whole image to validate it, then return the original bytes
/// unchanged: a valid header over corrupt data must not reach the model
/// request. Returns the image and its marker description, or why it is not
/// shown.
fn prepare(mime: &str, base64: &str) -> Result<(ReturnedImage, String), String> {
    let (format, mime, label) = match mime {
        "image/png" => (ImageFormat::Png, "image/png", "PNG"),
        "image/jpeg" => (ImageFormat::Jpeg, "image/jpeg", "JPEG"),
        other => return Err(format!("{other} is not a returned image type")),
    };
    // nbformat allows line-wrapped base64.
    let compact: String = base64.split_ascii_whitespace().collect();
    let bytes = STANDARD
        .decode(&compact)
        .map_err(|_| "invalid base64 data".to_string())?;
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(DECODE_MAX_EDGE);
    limits.max_image_height = Some(DECODE_MAX_EDGE);
    limits.max_alloc = Some(DECODE_MAX_ALLOC);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("could not be decoded ({error})"))?;
    Ok((
        ReturnedImage {
            mime,
            base64: compact,
        },
        format!("{}x{} {label}", decoded.width(), decoded.height()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> String {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            let v = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)).to_le_bytes();
            image::Rgba([v[0], v[1], v[2], 255])
        });
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        STANDARD.encode(out.into_inner())
    }

    #[test]
    fn images_pass_through_unchanged_whatever_their_size() {
        let mut images = ImageCollector::default();
        let small = png(64, 32);
        let wrapped = format!("{}\n{}", &small[..10], &small[10..]);
        assert_eq!(images.add("image/png", &wrapped), "[image 1: 64x32 PNG]");
        assert_eq!(images.images[0].base64, small);

        let large = png(3000, 2000);
        assert_eq!(images.add("image/png", &large), "[image 2: 3000x2000 PNG]");
        assert_eq!(images.images[1].base64, large);
    }

    #[test]
    fn cap_counts_images_left_out() {
        let data = png(8, 8);
        let mut images = ImageCollector::default();
        for _ in 0..MAX_IMAGES_PER_REPLY {
            images.add("image/png", &data);
        }
        assert!(images.footer().is_none());
        assert!(
            images
                .add("image/png", &data)
                .starts_with("[image not shown")
        );
        assert_eq!(images.images.len(), MAX_IMAGES_PER_REPLY);
        assert!(images.footer().unwrap().starts_with("1 more image(s)"));
    }

    #[test]
    fn garbage_is_reported_not_returned() {
        let mut images = ImageCollector::default();
        assert_eq!(
            images.add("image/png", "!!!"),
            "[image not shown: invalid base64 data]"
        );
        let marker = images.add("image/png", &STANDARD.encode(b"not a png"));
        assert!(
            marker.starts_with("[image not shown: could not be decoded"),
            "{marker}"
        );
        // A valid header over truncated pixel data is refused too.
        let whole = STANDARD.decode(png(64, 64)).unwrap();
        let truncated = STANDARD.encode(&whole[..whole.len() / 2]);
        let marker = images.add("image/png", &truncated);
        assert!(
            marker.starts_with("[image not shown: could not be decoded"),
            "{marker}"
        );
        assert!(images.images.is_empty());
    }
}
