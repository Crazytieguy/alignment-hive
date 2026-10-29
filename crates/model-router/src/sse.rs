//! Server-sent-event framing shared by every reader of a streamed response:
//! the usage transformer, the `WebSearch` tool-use sniffer, the xAI source
//! harvester, and the auto-mode classifier's stage. Events are reassembled
//! across arbitrary chunk boundaries.

pub(crate) fn event_boundary(buffer: &[u8]) -> Option<usize> {
    event_boundary_from(buffer, 0)
}

/// The end of the first complete event at or after `from`. The caller resumes
/// from where it last stopped so a long stream is not rescanned per chunk;
/// `from` must be rewound far enough to catch a separator split across chunks.
fn event_boundary_from(buffer: &[u8], from: usize) -> Option<usize> {
    (from..buffer.len()).find_map(|index| {
        if buffer[index..].starts_with(b"\r\n\r\n") {
            Some(index + 4)
        } else if buffer[index..].starts_with(b"\n\n") {
            Some(index + 2)
        } else {
            None
        }
    })
}

/// Reassembles SSE events from arbitrary chunk boundaries, unbounded: the
/// auto-mode classifier's stage bounds it itself, and [`SseEventBuffer`]
/// adds the passive readers' cap and latch.
#[derive(Default)]
pub(crate) struct SseFramer {
    buffer: Vec<u8>,
    /// How far the separator scan has already looked, so each chunk costs one
    /// pass over its own bytes rather than over the whole buffer.
    scanned: usize,
}

impl SseFramer {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    /// The bytes not yet taken as an event.
    pub(crate) const fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Runs `read` over the next complete event, which is then removed;
    /// `None` while no event is complete.
    pub(crate) fn next_with<T>(&mut self, read: impl FnOnce(&[u8]) -> T) -> Option<T> {
        // Rewind by the longest separator minus one so a separator split
        // across two chunks is still found.
        let resume = self.scanned.saturating_sub(3);
        let Some(event_end) = event_boundary_from(&self.buffer, resume) else {
            self.scanned = self.buffer.len();
            return None;
        };
        let read = read(&self.buffer[..event_end]);
        self.buffer.drain(..event_end);
        self.scanned = 0;
        Some(read)
    }

    pub(crate) fn next_event(&mut self) -> Option<Vec<u8>> {
        self.next_with(<[u8]>::to_vec)
    }

    /// Everything buffered, a complete event or not.
    pub(crate) fn take_rest(&mut self) -> Vec<u8> {
        self.scanned = 0;
        std::mem::take(&mut self.buffer)
    }
}

/// Overall cap on bytes buffered per scanned response; overflowing latches
/// the scanner off (forwarding, where there is any, is unaffected).
pub(crate) const MAX_SCAN_BUFFER_BYTES: usize = 2 * 1024 * 1024;

/// Reassembles SSE events from arbitrary chunk boundaries for the passive
/// readers (the `WebSearch` tool-use sniffer and the xAI source harvester).
/// Both need the same buffer, the same size cap, the same latch, and the same
/// drain loop; only what they do with each event differs.
#[derive(Default)]
pub(crate) struct SseEventBuffer {
    events: SseFramer,
    disabled: bool,
}

impl SseEventBuffer {
    /// Appends a chunk. Returns `false` once the buffer has outgrown its cap,
    /// after which the scanner is latched off and yields nothing.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> bool {
        if self.disabled {
            return false;
        }
        if self.events.buffered().saturating_add(chunk.len()) > MAX_SCAN_BUFFER_BYTES {
            self.disable();
            return false;
        }
        self.events.push(chunk);
        true
    }

    /// Runs `handle` over each complete event in arrival order, stopping at
    /// the first `Some`. Events are parsed in place; only consumed bytes are
    /// removed. Events that are not valid UTF-8 are skipped.
    pub(crate) fn drain<T>(&mut self, mut handle: impl FnMut(&str) -> Option<T>) -> Option<T> {
        while !self.disabled {
            let found = self
                .events
                .next_with(|event| std::str::from_utf8(event).ok().and_then(&mut handle))?;
            if found.is_some() {
                return found;
            }
        }
        None
    }

    pub(crate) fn disable(&mut self) {
        self.disabled = true;
        self.events = SseFramer::default();
    }

    pub(crate) const fn is_disabled(&self) -> bool {
        self.disabled
    }
}

/// `event` with its single `data:` payload (at `range`) replaced by `data`.
pub(crate) fn splice_data(event: &[u8], range: &std::ops::Range<usize>, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        event
            .len()
            .saturating_sub(range.len())
            .saturating_add(data.len()),
    );
    out.extend_from_slice(&event[..range.start]);
    out.extend_from_slice(data);
    out.extend_from_slice(&event[range.end..]);
    out
}

/// One parsed SSE event.
pub(crate) struct SseEvent<'a> {
    /// The `event:` name, absent when the event carried only `data:` lines
    /// (which SSE permits).
    pub name: Option<&'a str>,
    /// The payload, with multiple `data:` lines joined by newlines as the
    /// spec requires. Borrowed in the single-line case, which is every event
    /// in practice.
    pub data: std::borrow::Cow<'a, str>,
    /// Where the payload sits in the event — `Some` only for a single `data:`
    /// line, the one case a rewritten payload can be spliced back in.
    pub data_range: Option<std::ops::Range<usize>>,
}

/// Parses an SSE event's fields. `None` when the event carries no `data:`
/// line at all (comments, `id:`/`retry:`-only events).
pub(crate) fn parse_event(event: &str) -> Option<SseEvent<'_>> {
    let mut name = None;
    let mut first_range: Option<std::ops::Range<usize>> = None;
    let mut joined: Option<String> = None;
    let mut offset = 0;

    for line_with_ending in event.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix("event:") {
            name = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            // Measured off the trimmed value so an all-whitespace payload
            // (`data: ` keep-alives) yields an empty range, not an inverted one.
            let trimmed = value.trim();
            let leading_whitespace = value.len() - value.trim_start().len();
            let start = offset + "data:".len() + leading_whitespace;
            let range = start..(start + trimmed.len());
            match (&first_range, &mut joined) {
                (None, _) => first_range = Some(range),
                (Some(first), None) => {
                    joined = Some(format!("{}\n{}", &event[first.clone()], &event[range]));
                }
                (Some(_), Some(joined)) => {
                    joined.push('\n');
                    joined.push_str(&event[range]);
                }
            }
        }
        offset += line_with_ending.len();
    }

    let first_range = first_range?;
    Some(match joined {
        None => SseEvent {
            name,
            data: std::borrow::Cow::Borrowed(&event[first_range.clone()]),
            data_range: Some(first_range),
        },
        Some(joined) => SseEvent {
            name,
            data: std::borrow::Cow::Owned(joined),
            data_range: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_framer_resumes_across_chunks() {
        let mut framer = SseFramer::default();
        framer.push(b"event: a\ndata: 1\n");
        assert!(framer.next_event().is_none());
        framer.push(b"\nevent: b\r\n");
        assert_eq!(framer.next_event().unwrap(), b"event: a\ndata: 1\n\n");
        framer.push(b"data: 2\r\n\r\nrest");
        assert_eq!(framer.next_event().unwrap(), b"event: b\r\ndata: 2\r\n\r\n");
        assert!(framer.next_event().is_none());
        assert_eq!(framer.take_rest(), b"rest");
    }

    #[test]
    fn whitespace_only_data_lines_parse_as_empty_payloads() {
        let event = "event: ping\ndata: \n\n";
        let parsed = parse_event(event).unwrap();
        assert_eq!(parsed.name, Some("ping"));
        assert_eq!(parsed.data, "");
        assert_eq!(&event[parsed.data_range.unwrap()], "");

        // The multi-`data:` join walks the same ranges.
        let joined = parse_event("event: ping\ndata:\t\ndata: {}\n\n").unwrap();
        assert_eq!(joined.data, "\n{}");
    }
}
