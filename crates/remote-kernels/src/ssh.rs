use std::path::{Path, PathBuf};

use ssh_key::private::Ed25519Keypair;
use ssh_key::{LineEnding, PrivateKey};

/// Comment attached to plugin-generated keys so they are identifiable in
/// provider consoles (e.g. the vast.ai account key list).
pub const KEY_COMMENT: &str = "remote-kernels";

pub struct SshKeypair {
    pub public_key_openssh: String,
    pub private_key_path: PathBuf,
}

/// Re-derive the OpenSSH public key from a private key written by
/// [`generate_keypair`] (no `.pub` file is kept on disk).
pub fn public_key_for(key_path: &Path) -> anyhow::Result<String> {
    let private = PrivateKey::read_openssh_file(key_path)?;
    Ok(private.public_key().to_openssh()?)
}

fn new_private_key() -> PrivateKey {
    let keypair = Ed25519Keypair::random(&mut rand::thread_rng());
    let mut private_key = PrivateKey::from(keypair);
    private_key.set_comment(KEY_COMMENT);
    private_key
}

fn keypair_result(private_key: &PrivateKey, key_path: &Path) -> anyhow::Result<SshKeypair> {
    Ok(SshKeypair {
        public_key_openssh: private_key.public_key().to_openssh()?,
        private_key_path: key_path.to_path_buf(),
    })
}

/// Generate an Ed25519 SSH keypair for machine access.
///
/// The private key is written to `key_path` (overwriting any existing file).
/// The public key is returned as an OpenSSH-format string for injection into
/// the machine's environment.
pub fn generate_keypair(key_path: &Path) -> anyhow::Result<SshKeypair> {
    if let Some(dir) = key_path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let private_key = new_private_key();
    std::fs::write(key_path, private_key.to_openssh(LineEnding::LF)?.as_str())?;

    // Restrict permissions (owner read-only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
    }

    tracing::info!(?key_path, "Generated SSH keypair");
    keypair_result(&private_key, key_path)
}

/// Load the keypair at `key_path`, generating it if absent — for the plugin's
/// stable key, which must survive across instances and sessions.
///
/// Creation is `O_EXCL`-atomic: the in-process state lock serializes starts
/// within one server, but two Claude sessions share the same project state
/// dir, and a silent overwrite here would destroy the private half of a key
/// another server just registered. Losing the race falls back to loading the
/// winner's key.
pub fn ensure_keypair(key_path: &Path) -> anyhow::Result<SshKeypair> {
    use anyhow::Context as _;

    if let Some(dir) = key_path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    if !key_path.exists() {
        let private_key = new_private_key();
        let pem = private_key.to_openssh(LineEnding::LF)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(key_path) {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(pem.as_bytes())?;
                tracing::info!(?key_path, "Generated stable SSH keypair");
                return keypair_result(&private_key, key_path);
            }
            // Another process created it between the check and the open —
            // their key is the stable key now; load it below.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }

    Ok(SshKeypair {
        public_key_openssh: public_key_for(key_path).with_context(|| {
            format!(
                "unreadable stable SSH key at {} — delete the file to regenerate",
                key_path.display()
            )
        })?,
        private_key_path: key_path.to_path_buf(),
    })
}

/// Whether two OpenSSH public keys are the same key: compares the algorithm
/// and base64 material, ignoring the trailing comment (providers may strip or
/// rewrite it).
pub fn same_key_material(a: &str, b: &str) -> bool {
    fn material(key: &str) -> Option<(&str, &str)> {
        let mut fields = key.split_whitespace();
        Some((fields.next()?, fields.next()?))
    }
    match (material(a), material(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_keypair_is_stable_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        let first = ensure_keypair(&path).unwrap();
        let second = ensure_keypair(&path).unwrap();
        assert_eq!(first.public_key_openssh, second.public_key_openssh);
    }

    #[test]
    fn generated_key_carries_plugin_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        let kp = generate_keypair(&path).unwrap();
        assert!(kp.public_key_openssh.ends_with(KEY_COMMENT));
        // Re-derivation from disk preserves it.
        assert_eq!(public_key_for(&path).unwrap(), kp.public_key_openssh);
    }

    #[test]
    fn generate_keypair_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        let first = generate_keypair(&path).unwrap();
        let second = generate_keypair(&path).unwrap();
        assert_ne!(first.public_key_openssh, second.public_key_openssh);
    }

    #[test]
    fn same_key_material_ignores_comment() {
        let a = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKx7 remote-kernels";
        let b = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKx7";
        let c = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDIF other";
        assert!(same_key_material(a, b));
        assert!(!same_key_material(a, c));
        assert!(!same_key_material(a, ""));
    }
}
