use std::path::{Path, PathBuf};

use ssh_key::private::Ed25519Keypair;
use ssh_key::{LineEnding, PrivateKey};

pub struct SshKeypair {
    pub public_key_openssh: String,
    pub private_key_path: PathBuf,
}

/// Generate an ephemeral Ed25519 SSH keypair for machine access.
///
/// The private key is written to `key_path` (one key per instance, inside its
/// state dir). The public key is returned as an OpenSSH-format string for
/// injection into the machine's environment.
pub fn generate_keypair(key_path: &Path) -> anyhow::Result<SshKeypair> {
    if let Some(dir) = key_path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let keypair = Ed25519Keypair::random(&mut rand::thread_rng());
    let private_key = PrivateKey::from(keypair);

    let private_pem = private_key.to_openssh(LineEnding::LF)?;
    std::fs::write(key_path, private_pem.as_str())?;

    // Restrict permissions (owner read-only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
    }

    let public_key_openssh = private_key.public_key().to_openssh()?;

    tracing::info!(?key_path, "Generated ephemeral SSH keypair");

    Ok(SshKeypair {
        public_key_openssh,
        private_key_path: key_path.to_path_buf(),
    })
}
