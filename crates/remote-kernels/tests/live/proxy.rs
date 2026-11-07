//! A tiny CONNECT proxy that loses a provider's answer to the one request a
//! money-safety test cares about, with no change to the server under test:
//! the server's HTTP client honors `HTTPS_PROXY`, so this sits on the path of
//! every provider call.
//!
//! The traffic is TLS, so the request can only be recognized by shape. A
//! machine-create body carries the whole start script and env, and is the only
//! request in a provisioning that fills a large application-data record; every
//! GET and small POST sits well under [`LARGE_RECORD`]. Whether that request made
//! a machine (a capacity refusal on the way down a GPU ladder doesn't) is asked of
//! the provider, while the answer waits here.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const LARGE_RECORD: usize = 900;
const TLS_APPLICATION_DATA: u8 = 0x17;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Forward the create, then cut the connection as its answer arrives.
    Drop,
    /// Forward the create, then sit on the answer until the client gives up.
    Hold,
}

type Created = dyn Fn() -> bool + Send + Sync;

pub struct ConnectProxy {
    pub url: String,
    triggered: Arc<AtomicBool>,
}

/// A silence this long after a large request means its answer has all arrived.
const ANSWER_QUIET: Duration = Duration::from_millis(600);

impl ConnectProxy {
    /// `created` asks the provider whether the machine exists yet. An answer
    /// that came with no machine passes through, and the proxy waits for the
    /// next create.
    pub fn start(
        mode: Mode,
        created: impl Fn() -> bool + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let triggered = Arc::new(AtomicBool::new(false));
        let created: Arc<Created> = Arc::new(created);
        {
            let triggered = Arc::clone(&triggered);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let triggered = Arc::clone(&triggered);
                    let created = Arc::clone(&created);
                    std::thread::spawn(move || {
                        let _ = serve(stream, mode, &created, &triggered);
                    });
                }
            });
        }
        Ok(Self { url, triggered })
    }

    /// Whether a create has gone through and its answer has been withheld.
    pub fn triggered(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }
}

fn serve(
    mut client: TcpStream,
    mode: Mode,
    created: &Arc<Created>,
    triggered: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(client.read(&mut byte)? == 1, "client closed before CONNECT");
        head.push(byte[0]);
        anyhow::ensure!(head.len() < 8192, "oversized CONNECT head");
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let target = head
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("no CONNECT target"))?
        .to_string();
    let upstream = TcpStream::connect(&target)?;
    client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")?;
    eprintln!("[proxy] CONNECT {target}");

    let armed = Arc::new(AtomicBool::new(false));
    {
        let from = upstream.try_clone()?;
        let to = client.try_clone()?;
        let armed = Arc::clone(&armed);
        let triggered = Arc::clone(triggered);
        let created = Arc::clone(created);
        std::thread::spawn(move || relay_answers(from, to, mode, &*created, &armed, &triggered));
    }

    // Client to upstream: forward everything, watching the TLS record headers.
    let mut to = upstream;
    let mut from = client;
    from.set_read_timeout(Some(Duration::from_secs(600)))?;
    let mut framer = RecordFramer::default();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = from.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let large = framer.feed(&buffer[..count]);
        to.write_all(&buffer[..count])?;
        if large && !triggered.load(Ordering::SeqCst) {
            eprintln!("[proxy] large request: {count} bytes read; waiting for its answer");
            armed.store(true, Ordering::SeqCst);
        }
    }
    let _ = to.shutdown(Shutdown::Write);
    Ok(())
}

/// Upstream to client: the half the fault is injected into. While armed, an
/// answer is buffered until it goes quiet, then judged by its size.
fn relay_answers(
    mut from: TcpStream,
    mut to: TcpStream,
    mode: Mode,
    created: &Created,
    armed: &AtomicBool,
    triggered: &AtomicBool,
) {
    if from
        .set_read_timeout(Some(Duration::from_millis(100)))
        .is_err()
    {
        return;
    }
    let mut buffer = [0u8; 16 * 1024];
    let mut answer = Vec::new();
    let mut last_read = Instant::now();
    let mut swallowing = false;
    loop {
        match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) if swallowing => {}
            Ok(count) if armed.load(Ordering::SeqCst) => {
                answer.extend_from_slice(&buffer[..count]);
                last_read = Instant::now();
            }
            Ok(count) => {
                if to.write_all(&buffer[..count]).is_err() {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if answer.is_empty() || last_read.elapsed() < ANSWER_QUIET {
                    continue;
                }
                // Listing lags creating by a moment; the client can wait for this.
                let machine_exists = (0..6).any(|attempt| {
                    if attempt > 0 {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    created()
                });
                eprintln!(
                    "[proxy] answer of {} bytes; machine exists: {machine_exists}",
                    answer.len()
                );
                if !machine_exists {
                    armed.store(false, Ordering::SeqCst);
                    if to.write_all(&answer).is_err() {
                        break;
                    }
                    answer.clear();
                    continue;
                }
                triggered.store(true, Ordering::SeqCst);
                if mode == Mode::Drop {
                    let _ = to.shutdown(Shutdown::Both);
                    let _ = from.shutdown(Shutdown::Both);
                    return;
                }
                // Hold: keep reading so the upstream stays happy.
                swallowing = true;
                answer.clear();
            }
            Err(_) => break,
        }
    }
    let _ = to.shutdown(Shutdown::Write);
}

/// Walks a TLS byte stream record by record.
#[derive(Default)]
struct RecordFramer {
    header: Vec<u8>,
    remaining_body: usize,
}

impl RecordFramer {
    /// Returns whether a large application-data record started in `bytes`.
    fn feed(&mut self, mut bytes: &[u8]) -> bool {
        let mut large = false;
        while !bytes.is_empty() {
            if self.remaining_body > 0 {
                let skip = self.remaining_body.min(bytes.len());
                self.remaining_body -= skip;
                bytes = &bytes[skip..];
                continue;
            }
            let need = 5 - self.header.len();
            let take = need.min(bytes.len());
            self.header.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.header.len() == 5 {
                let length = usize::from(u16::from_be_bytes([self.header[3], self.header[4]]));
                large |= self.header[0] == TLS_APPLICATION_DATA && length >= LARGE_RECORD;
                self.remaining_body = length;
                self.header.clear();
            }
        }
        large
    }
}

#[test]
fn framer_finds_large_application_data_records_across_reads() {
    let mut framer = RecordFramer::default();
    // A large ClientHello is not application data.
    let mut hello = vec![0x16, 3, 1, 0x06, 0x00];
    hello.resize(5 + 0x0600, 0);
    assert!(!framer.feed(&hello));
    // A small application-data record.
    let mut small = vec![TLS_APPLICATION_DATA, 3, 3, 0x00, 0x40];
    small.resize(5 + 0x40, 0);
    assert!(!framer.feed(&small));
    // A large one, split inside its header.
    let mut record = vec![TLS_APPLICATION_DATA, 3, 3, 0x08, 0x00];
    record.resize(5 + 0x0800, 0);
    assert!(!framer.feed(&record[..3]));
    assert!(framer.feed(&record[3..]));
}
