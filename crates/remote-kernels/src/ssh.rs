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

/// Fail-closed check that a private key file is actually usable by OpenSSH:
/// on unix its mode must have no group/other bits. On filesystems where chmod
/// is a silent no-op (a WSL `/mnt/c` drvfs mount without the `metadata`
/// option, FAT drives), the key sits at an effective 0777 and OpenSSH ignores
/// it — auth then fails with nothing pointing at the key file. Every path
/// that is about to hand a key to `ssh` (fresh generation, load, and
/// record-based reconnect/attach — the latter BEFORE any billing resume)
/// calls this so the user gets the cause and the fix instead.
// The octal mask mirrors how OpenSSH states the rule; a trailing_zeros
// comparison would obscure it.
#[allow(clippy::verbose_bit_mask)]
pub fn validate_private_key_file(key_path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(key_path).map_err(|error| {
        anyhow::anyhow!(
            "SSH private key {} is unusable: {error}",
            key_path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        anyhow::ensure!(
            mode & 0o077 == 0,
            "SSH private key {} has mode {mode:o} (group/other-accessible), so OpenSSH \
             will refuse to use it. This usually means the file sits on a filesystem \
             that cannot enforce permissions — e.g. a WSL /mnt/c mount. Fix: remount \
             the drive with the `metadata` option (or move the key to a Linux \
             filesystem and update the record) and chmod 600 the file. Do NOT delete \
             or regenerate the key — a machine that already uses it would become \
             unreachable.",
            key_path.display(),
        );
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

/// Write private-key material with owner-only access from the first byte
/// (`create_new` + mode 0600 — never write-then-chmod, which leaves a
/// permissive window under ordinary umasks). With `overwrite`, an existing
/// file is removed first; without it, `AlreadyExists` is returned untouched
/// for the caller's cross-session race handling.
fn write_key_file(key_path: &Path, pem: &[u8], overwrite: bool) -> std::io::Result<()> {
    use std::io::Write as _;
    if overwrite {
        match std::fs::remove_file(key_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(key_path)?;
    file.write_all(pem)
}

/// Copy private-key bytes to a new location, owner-only from the first byte.
/// Migration helper: an existing machine's key must keep its exact material
/// (the public half is in the machine's `authorized_keys` / provider
/// registry). A destination that already holds the SAME bytes is success (a
/// re-run migration or a concurrent session won the race); different bytes is
/// an error — key material is never overwritten.
///
/// Crash-atomic: the bytes are staged in a private temp file (synced), then
/// installed via `hard_link`, which never replaces an existing destination.
/// A death at any point leaves either no destination or a complete one —
/// never a partial key that would poison every later migration attempt.
pub fn copy_key_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(src)?;
    let dir = dst
        .parent()
        .ok_or_else(|| anyhow::anyhow!("key destination {} has no parent", dst.display()))?;
    std::fs::create_dir_all(dir)?;
    if !dst.exists() {
        let staging = dir.join(format!(".key-staging.{}", std::process::id()));
        write_key_file(&staging, &bytes, true)?;
        std::fs::File::open(&staging)?.sync_all()?;
        let linked = std::fs::hard_link(&staging, dst);
        let _ = std::fs::remove_file(&staging);
        match linked {
            Ok(()) => {
                // Directory entry durable before the caller can persist a
                // record pointing at it.
                std::fs::File::open(dir)?.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::ensure!(
        std::fs::read(dst)? == bytes,
        "destination {} already holds a different key",
        dst.display()
    );
    validate_private_key_file(dst)
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
    write_key_file(
        key_path,
        private_key.to_openssh(LineEnding::LF)?.as_bytes(),
        true,
    )?;
    validate_private_key_file(key_path)?;

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
        match write_key_file(key_path, pem.as_bytes(), false) {
            Ok(()) => {
                validate_private_key_file(key_path)?;
                tracing::info!(?key_path, "Generated stable SSH keypair");
                return keypair_result(&private_key, key_path);
            }
            // Another process created it between the check and the open —
            // their key is the stable key now; load it below.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }

    validate_private_key_file(key_path)?;
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

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Keys must be owner-only from the first byte — never write-then-chmod.
    #[cfg(unix)]
    #[test]
    fn keys_are_created_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let generated = dir.path().join("per-instance");
        generate_keypair(&generated).unwrap();
        assert_eq!(mode_of(&generated) & 0o077, 0);
        let stable = dir.path().join("stable");
        ensure_keypair(&stable).unwrap();
        assert_eq!(mode_of(&stable) & 0o077, 0);
    }

    /// A group/other-accessible key is one OpenSSH will silently ignore
    /// (the WSL /mnt/c failure) — loading it must fail loudly, before any
    /// machine is allocated or resumed.
    #[cfg(unix)]
    #[test]
    fn insecure_key_mode_fails_closed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        generate_keypair(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = match ensure_keypair(&path) {
            Ok(_) => panic!("insecure key must not load"),
            Err(error) => format!("{error:#}"),
        };
        assert!(error.contains("group/other-accessible"), "{error}");
        assert!(validate_private_key_file(&path).is_err());
        // Missing file is also a validation error, not a panic.
        assert!(validate_private_key_file(&dir.path().join("absent")).is_err());
    }

    /// Migration copies key material byte-exact and never overwrites a
    /// different key already at the destination.
    #[test]
    fn copy_key_file_preserves_material_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        generate_keypair(&src).unwrap();
        let dst = dir.path().join("nested/dst");
        copy_key_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        // Re-running (or losing a cross-session race to the same bytes) is
        // fine...
        copy_key_file(&src, &dst).unwrap();
        // ...but a different key at the destination is untouchable.
        let other = dir.path().join("other");
        generate_keypair(&other).unwrap();
        let error = format!("{:#}", copy_key_file(&other, &dst).unwrap_err());
        assert!(error.contains("different key"), "{error}");
        assert_eq!(std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
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
