//! A pooled HTTP/2 upstream connection that goes silent without closing.
//!
//! The upstream is a TLS HTTP/2 server behind a relay that can freeze the
//! connections it already carries: they stay open but pass no more bytes,
//! as when a laptop wakes on another network or a middlebox drops the flow.
//! Connections opened after the freeze work.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

use super::upstream_client;

#[derive(Default)]
struct Relay {
    opened: AtomicU64,
    frozen_below: AtomicU64,
}

impl Relay {
    fn freeze_open_connections(&self) {
        self.frozen_below
            .store(self.opened.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    fn is_frozen(&self, id: u64) -> bool {
        id < self.frozen_below.load(Ordering::SeqCst)
    }
}

async fn pipe(relay: Arc<Relay>, id: u64, mut from: OwnedReadHalf, mut to: OwnedWriteHalf) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if relay.is_frozen(id) {
            // Silent, not closed: both sockets stay open.
            std::future::pending::<()>().await;
        }
        if to.write_all(&buf[..n]).await.is_err() {
            break;
        }
    }
    let _ = to.shutdown().await;
}

/// Starts the upstream and its relay; returns the URL to reach it through
/// the relay, the CA that signed its certificate, and the relay.
async fn spawn_upstream() -> (String, reqwest::Certificate, Arc<Relay>) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "keepalive test CA");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let mut leaf_params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    leaf_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
    )
    .unwrap();
    tls.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));

    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = server.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = server.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let service = hyper::service::service_fn(|request: hyper::Request<_>| async move {
                    if request.uri().path() == "/slow" {
                        // Several ping intervals of server think time.
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                    }
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(axum::body::Body::from(
                        "ok",
                    )))
                });
                let _ =
                    hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection(hyper_util::rt::TokioIo::new(tls), service)
                        .await;
            });
        }
    });

    let relay = Arc::new(Relay::default());
    let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = front.local_addr().unwrap().port();
    let accepting = Arc::clone(&relay);
    tokio::spawn(async move {
        loop {
            let (client, _) = front.accept().await.unwrap();
            let upstream = tokio::net::TcpStream::connect(server_addr).await.unwrap();
            let id = accepting.opened.fetch_add(1, Ordering::SeqCst);
            let (client_read, client_write) = client.into_split();
            let (upstream_read, upstream_write) = upstream.into_split();
            tokio::spawn(pipe(
                Arc::clone(&accepting),
                id,
                client_read,
                upstream_write,
            ));
            tokio::spawn(pipe(
                Arc::clone(&accepting),
                id,
                upstream_read,
                client_write,
            ));
        }
    });

    let ca = reqwest::Certificate::from_pem(ca_cert.pem().as_bytes()).unwrap();
    (format!("https://localhost:{port}/"), ca, relay)
}

/// The client under test, trusting the upstream's CA and ignoring any proxy
/// the test process inherited.
fn client(builder: reqwest::ClientBuilder, ca: reqwest::Certificate) -> reqwest::Client {
    builder.no_proxy().add_root_certificate(ca).build().unwrap()
}

/// One request with the deadline a caller such as Claude Code would give it.
/// A failure reads as its whole error chain.
async fn get(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let request = async { client.get(url).send().await?.text().await };
    match tokio::time::timeout(Duration::from_secs(3), request).await {
        Err(_) => Err("hung".to_string()),
        Ok(Err(error)) => {
            let mut chain = format!("failed: {error}");
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                chain = format!("{chain}: {cause}");
                source = cause.source();
            }
            Err(chain)
        }
        Ok(Ok(body)) => Ok(body),
    }
}

#[tokio::test]
async fn a_silent_connection_is_dropped_and_the_next_request_reconnects() {
    let (url, ca, relay) = spawn_upstream().await;
    let client = client(
        upstream_client(Duration::from_millis(300), Duration::from_millis(300)),
        ca,
    );
    assert_eq!(get(&client, &url).await.as_deref(), Ok("ok"));
    relay.freeze_open_connections();

    // The request already sent on the silent connection fails once the
    // ping goes unanswered, well before the caller's deadline.
    let on_silent = get(&client, &url).await;
    assert!(
        on_silent
            .as_ref()
            .is_err_and(|error| error.contains("keep-alive timed out")),
        "{on_silent:?}"
    );
    // The next one opens a fresh connection.
    assert_eq!(get(&client, &url).await.as_deref(), Ok("ok"));
    assert_eq!(relay.opened.load(Ordering::SeqCst), 2);
}

/// An idle connection that goes silent is dropped before the next request
/// would reuse it, so that request succeeds first try.
#[tokio::test]
async fn an_idle_silent_connection_is_gone_before_the_next_request() {
    let (url, ca, relay) = spawn_upstream().await;
    let client = client(
        upstream_client(Duration::from_millis(300), Duration::from_millis(300)),
        ca,
    );
    assert_eq!(get(&client, &url).await.as_deref(), Ok("ok"));
    relay.freeze_open_connections();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(get(&client, &url).await.as_deref(), Ok("ok"));
    assert_eq!(relay.opened.load(Ordering::SeqCst), 2);
}

/// The failure the pings prevent: without them, every request after the
/// connection goes silent is sent on it and hangs.
#[tokio::test]
async fn without_pings_every_request_hangs_on_a_silent_connection() {
    let (url, ca, relay) = spawn_upstream().await;
    let client = client(
        upstream_client(Duration::from_millis(300), Duration::from_millis(300))
            .http2_keep_alive_interval(None),
        ca,
    );
    assert_eq!(get(&client, &url).await.as_deref(), Ok("ok"));
    relay.freeze_open_connections();

    assert_eq!(get(&client, &url).await, Err("hung".to_string()));
    assert_eq!(get(&client, &url).await, Err("hung".to_string()));
    assert_eq!(relay.opened.load(Ordering::SeqCst), 1);
}

/// A live upstream that takes several ping intervals to answer keeps its
/// connection, idle or busy: the pings are answered meanwhile.
#[tokio::test]
async fn a_slow_but_live_upstream_keeps_its_connection() {
    let (url, ca, relay) = spawn_upstream().await;
    let client = client(
        upstream_client(Duration::from_millis(300), Duration::from_millis(300)),
        ca,
    );
    assert_eq!(get(&client, &url).await.as_deref(), Ok("ok"));
    assert_eq!(
        get(&client, &format!("{url}slow")).await.as_deref(),
        Ok("ok")
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(get(&client, &url).await.as_deref(), Ok("ok"));
    assert_eq!(relay.opened.load(Ordering::SeqCst), 1);
}
