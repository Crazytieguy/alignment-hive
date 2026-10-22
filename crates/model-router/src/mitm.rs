//! The local certificate authority behind the HTTPS-proxy front (the CONNECT
//! handling in [`crate::proxy`]): a create-once CA in the state dir,
//! name-constrained to the one intercepted host, and the TLS server config
//! intercepted tunnels are terminated with, around a leaf for that host
//! minted at startup.
//!
//! Claude Code trusts the CA through the login keychain on macOS
//! ([`trust`]), which the desktop app's own CA bundle is built from too, or
//! through `NODE_EXTRA_CA_CERTS` in its settings elsewhere; the name
//! constraint keeps a leaked key from vouching for any other host.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use axum::http::HeaderValue;

use crate::state::{Dirs, create_private_dir, write_private_atomic};

/// The only host whose tunnels are terminated; every other CONNECT is a
/// blind tunnel.
pub const INTERCEPTED_HOST: &str = "api.anthropic.com";
pub const INTERCEPTED_PORT: u16 = 443;

const CA_CERT_FILE: &str = "ca.pem";
const CA_KEY_FILE: &str = "ca-key.pem";
const CA_COMMON_NAME: &str = "model-router local CA (api.anthropic.com only)";

/// The leaf's lifetime. Re-minted on every start; Apple's 398-day ceiling
/// for TLS leaves is the bound that matters.
const LEAF_VALIDITY_DAYS: i64 = 397;
const CA_VALIDITY_DAYS: i64 = 3650;

/// The CA directory under the state dir.
#[must_use]
pub fn ca_dir(dirs: &Dirs) -> PathBuf {
    dirs.state_dir.join("mitm")
}

/// The CA certificate users trust (keychain or `NODE_EXTRA_CA_CERTS`).
#[must_use]
pub fn ca_cert_path(ca_dir: &Path) -> PathBuf {
    ca_dir.join(CA_CERT_FILE)
}

/// The `Proxy-Authorization` value Claude Code's `proxyAuthHelper` prints.
/// Only Claude Code sends it: the processes it spawns inherit `HTTPS_PROXY`
/// but not the helper, so their tunnels stay blind.
#[must_use]
pub fn proxy_authorization(token: &str) -> String {
    format!("Bearer {token}")
}

/// What the CONNECT handler needs to terminate an intercepted tunnel.
pub struct Interceptor {
    acceptor: tokio_rustls::TlsAcceptor,
    credential: String,
    ca_cert: PathBuf,
}

impl std::fmt::Debug for Interceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interceptor")
            .field("ca_cert", &self.ca_cert)
            .finish_non_exhaustive()
    }
}

impl Interceptor {
    /// Loads the CA from `ca_dir` (creating it on first use) and mints the
    /// leaf for [`INTERCEPTED_HOST`].
    ///
    /// # Errors
    /// Returns an error when the CA cannot be read, created or parsed, or the
    /// leaf cannot be signed.
    pub fn load(ca_dir: &Path, token: &str) -> anyhow::Result<Self> {
        let key_pem = load_or_create_ca(ca_dir)?;
        let ca_key = rcgen::KeyPair::from_pem(&key_pem).context("unreadable CA key")?;
        // The issuer is the CA's constant params around its stored key: the
        // leaf names the stored certificate's subject, and its authority key
        // identifier is the same hash of the same public key.
        let issuer = rcgen::Issuer::new(ca_params(), ca_key);

        let leaf_key = rcgen::KeyPair::generate()?;
        let mut params = rcgen::CertificateParams::new(vec![INTERCEPTED_HOST.to_string()])?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, INTERCEPTED_HOST);
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        (params.not_before, params.not_after) = validity(LEAF_VALIDITY_DAYS);
        let leaf = params.signed_by(&leaf_key, &issuer)?;

        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![leaf.der().clone()], key)?;
        // The handler serves HTTP/1.1 only.
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];

        Ok(Self {
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(tls)),
            credential: proxy_authorization(token),
            ca_cert: ca_cert_path(ca_dir),
        })
    }

    #[must_use]
    pub fn acceptor(&self) -> &tokio_rustls::TlsAcceptor {
        &self.acceptor
    }

    #[must_use]
    pub fn credential_matches(&self, value: &HeaderValue) -> bool {
        value.as_bytes() == self.credential.as_bytes()
    }

    #[must_use]
    pub fn ca_cert(&self) -> &Path {
        &self.ca_cert
    }
}

/// The CA certificate in `ca_dir`, created first if absent.
///
/// # Errors
/// Returns an error when the CA cannot be read or created.
pub fn ensure_ca(ca_dir: &Path) -> anyhow::Result<PathBuf> {
    load_or_create_ca(ca_dir)?;
    Ok(ca_cert_path(ca_dir))
}

/// `(not_before, not_after)`: from a day ago, to cover clock skew, for `days`.
fn validity(days: i64) -> (time::OffsetDateTime, time::OffsetDateTime) {
    let now = time::OffsetDateTime::now_utc();
    (
        now - time::Duration::days(1),
        now + time::Duration::days(days),
    )
}

/// Keeps one process's staging dirs apart (tests create CAs in parallel).
static STAGINGS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Reads the CA's key, or creates the CA when neither file exists. One file
/// without the other is an error rather than a silent replacement: the user
/// trusted the certificate that is there.
fn load_or_create_ca(ca_dir: &Path) -> anyhow::Result<String> {
    let cert_path = ca_cert_path(ca_dir);
    let key_path = ca_dir.join(CA_KEY_FILE);
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => std::fs::read_to_string(&key_path)
            .with_context(|| format!("failed to read {}", key_path.display())),
        (false, false) => {
            let key = rcgen::KeyPair::generate()?;
            let mut params = ca_params();
            (params.not_before, params.not_after) = validity(CA_VALIDITY_DAYS);
            let cert = params.self_signed(&key)?;
            // Both files appear at once, in a directory renamed into place:
            // the service and `trust-ca` may both get here first.
            let parent = ca_dir.parent().context("CA dir has no parent")?;
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            let staging = parent.join(format!(
                ".mitm-{}-{}",
                std::process::id(),
                STAGINGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&staging);
            create_private_dir(&staging)?;
            write_private_atomic(&staging.join(CA_KEY_FILE), key.serialize_pem().as_bytes())?;
            let staged_cert = staging.join(CA_CERT_FILE);
            write_private_atomic(&staged_cert, cert.pem().as_bytes())?;
            // What the user trusts is no secret, and a tool reading it as
            // another user (a Node child) must not fail on it.
            std::fs::set_permissions(&staged_cert, std::fs::Permissions::from_mode(0o644))?;
            if std::fs::rename(&staging, ca_dir).is_err() {
                // Another process created it meanwhile: theirs stands.
                let _ = std::fs::remove_dir_all(&staging);
                return std::fs::read_to_string(&key_path)
                    .with_context(|| format!("failed to read {}", key_path.display()));
            }
            tracing::info!(path = %cert_path.display(), "created the HTTPS-proxy CA");
            Ok(key.serialize_pem())
        }
        _ => anyhow::bail!(
            "{} holds only one of {CA_CERT_FILE} and {CA_KEY_FILE}; remove the other (and \
             its keychain trust) to have a new CA created",
            ca_dir.display()
        ),
    }
}

/// The CA's certificate params, less its validity. Constant: a later start
/// signs with them, and the stored certificate must vouch for its leaves.
fn ca_params() -> rcgen::CertificateParams {
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, CA_COMMON_NAME);
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "model-router");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    params.name_constraints = Some(rcgen::NameConstraints {
        permitted_subtrees: vec![rcgen::GeneralSubtree::DnsName(INTERCEPTED_HOST.to_string())],
        excluded_subtrees: Vec::new(),
    });
    params
}

/// The login keychain, where [`trust`] adds the CA.
fn login_keychain() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Keychains/login.keychain-db"))
}

/// The certificate's SHA-1, in the uppercase hex the keychain keys trust
/// settings by.
fn sha1_hex(ca_cert: &Path) -> anyhow::Result<String> {
    use rustls::pki_types::pem::PemObject as _;
    use sha1::Digest as _;

    let der = rustls::pki_types::CertificateDer::from_pem_file(ca_cert)
        .with_context(|| format!("unreadable certificate {}", ca_cert.display()))?;
    Ok(sha1::Sha1::digest(der.as_ref())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02X}");
            hex
        }))
}

fn security(args: &[&std::ffi::OsStr]) -> anyhow::Result<std::process::Output> {
    std::process::Command::new("/usr/bin/security")
        .args(args)
        .output()
        .context("failed to run /usr/bin/security")
}

/// Whether the login keychain trusts `ca_cert` for TLS servers (macOS): its
/// hash among the user-domain trust settings, without a deny.
///
/// # Errors
/// Returns an error when the certificate or the trust settings cannot be
/// read.
pub fn is_trusted(ca_cert: &Path) -> anyhow::Result<bool> {
    let hash = sha1_hex(ca_cert)?;
    let dir = std::env::temp_dir().join(format!("model-router-trust-{}", std::process::id()));
    create_private_dir(&dir)?;
    let export = dir.join("trust.plist");
    let output = security(&["trust-settings-export".as_ref(), export.as_os_str()]);
    let plist = std::fs::read_to_string(&export);
    let _ = std::fs::remove_dir_all(&dir);
    // No user-domain trust settings at all is a failed export.
    if !output?.status.success() {
        return Ok(false);
    }
    Ok(trusts(&plist?, &hash))
}

/// [`is_trusted`]'s reading of an exported trust-settings plist: `hash`'s
/// entry exists and no setting in it denies (result 3) or leaves
/// unspecified (4).
fn trusts(plist: &str, hash: &str) -> bool {
    let key = format!("<key>{hash}</key>");
    let Some(start) = plist.find(&key) else {
        return false;
    };
    // The entry runs to the next certificate's key, at the same depth.
    let entry = &plist[start + key.len()..];
    let end = entry.find("\n\t\t<key>").unwrap_or(entry.len());
    let entry: String = entry[..end].split_whitespace().collect();
    !entry.contains("<key>kSecTrustSettingsResult</key><integer>3</integer>")
        && !entry.contains("<key>kSecTrustSettingsResult</key><integer>4</integer>")
}

/// Adds the CA to the login keychain, trusted for TLS servers (macOS). macOS
/// asks the person to authorize the change.
///
/// # Errors
/// Returns an error when `security` refuses, e.g. the person cancelled.
pub fn trust(ca_cert: &Path) -> anyhow::Result<()> {
    let keychain = login_keychain()?;
    let output = security(&[
        "add-trusted-cert".as_ref(),
        "-r".as_ref(),
        "trustRoot".as_ref(),
        "-p".as_ref(),
        "ssl".as_ref(),
        "-k".as_ref(),
        keychain.as_os_str(),
        ca_cert.as_os_str(),
    ])?;
    anyhow::ensure!(
        output.status.success(),
        "security add-trusted-cert failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// Removes the CA's trust and the certificate from the login keychain
/// (macOS). Nothing to remove is not an error.
///
/// # Errors
/// Returns an error when the certificate cannot be read or `security`
/// refuses the trust removal.
pub fn untrust(ca_cert: &Path) -> anyhow::Result<()> {
    let hash = sha1_hex(ca_cert)?;
    if is_trusted(ca_cert)? {
        let output = security(&["remove-trusted-cert".as_ref(), ca_cert.as_os_str()])?;
        anyhow::ensure!(
            output.status.success(),
            "security remove-trusted-cert failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let keychain = login_keychain()?;
    // Absent already is fine: `delete-certificate` then fails, and is ignored.
    let _ = security(&[
        "delete-certificate".as_ref(),
        "-Z".as_ref(),
        hash.as_ref(),
        keychain.as_os_str(),
    ])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reuses_the_ca() {
        let dir = tempfile::tempdir().unwrap();
        let ca_dir = dir.path().join("mitm");
        let first = Interceptor::load(&ca_dir, "abc").unwrap();
        let pem = std::fs::read_to_string(first.ca_cert()).unwrap();
        let second = Interceptor::load(&ca_dir, "abc").unwrap();
        assert_eq!(pem, std::fs::read_to_string(second.ca_cert()).unwrap());
        assert!(first.credential_matches(&HeaderValue::from_static("Bearer abc")));
        assert!(!first.credential_matches(&HeaderValue::from_static("Bearer abd")));
        let mode = std::fs::metadata(first.ca_cert())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
        let key = std::fs::metadata(ca_dir.join(CA_KEY_FILE)).unwrap();
        assert_eq!(key.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn refuses_a_half_present_ca() {
        let dir = tempfile::tempdir().unwrap();
        Interceptor::load(dir.path(), "abc").unwrap();
        std::fs::remove_file(dir.path().join(CA_KEY_FILE)).unwrap();
        assert!(Interceptor::load(dir.path(), "abc").is_err());
    }

    #[test]
    fn hashes_the_certificate_der() {
        let dir = tempfile::tempdir().unwrap();
        let interceptor = Interceptor::load(dir.path(), "abc").unwrap();
        let hash = sha1_hex(interceptor.ca_cert()).unwrap();
        assert_eq!(hash.len(), 40);
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
        );
    }

    const EXPORT: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<plist version=\"1.0\">
<dict>
\t<key>trustList</key>
\t<dict>
\t\t<key>AAAA</key>
\t\t<dict>
\t\t\t<key>trustSettings</key>
\t\t\t<array>
\t\t\t\t<dict>
\t\t\t\t\t<key>kSecTrustSettingsResult</key>
\t\t\t\t\t<integer>3</integer>
\t\t\t\t</dict>
\t\t\t</array>
\t\t</dict>
\t\t<key>BBBB</key>
\t\t<dict>
\t\t\t<key>trustSettings</key>
\t\t\t<array>
\t\t\t\t<dict>
\t\t\t\t\t<key>kSecTrustSettingsPolicyName</key>
\t\t\t\t\t<string>sslServer</string>
\t\t\t\t</dict>
\t\t\t</array>
\t\t</dict>
\t</dict>
</dict>
</plist>
";

    #[test]
    fn reads_trust_from_an_export() {
        assert!(trusts(EXPORT, "BBBB"));
        assert!(!trusts(EXPORT, "AAAA"), "a deny is not trust");
        assert!(!trusts(EXPORT, "CCCC"), "absent is not trust");
    }
}
