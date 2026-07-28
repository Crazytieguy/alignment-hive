//! State, config, and cache directory layout plus the file-hygiene
//! primitives shared by supervision, service management, and setup: `0700`
//! directories, `0600` create-once secrets, atomic writes, and the
//! single-instance lock.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context;

/// Filesystem layout for everything model-router owns outside the repo.
///
/// XDG-style on both macOS and Linux (deliberately not
/// `~/Library/Application Support` — one layout to document and debug).
/// The user's home directory, treating an empty `HOME` as unset. One
/// implementation so every caller agrees on that edge case.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

#[derive(Clone, Debug)]
pub struct Dirs {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Dirs {
    /// Resolves directories from the environment (`XDG_*` overrides
    /// respected, `$HOME` fallback).
    ///
    /// # Errors
    /// Returns an error when `$HOME` is unset and no XDG override covers a
    /// directory.
    pub fn resolve() -> anyhow::Result<Self> {
        let home = home_dir();
        let base = |xdg: &str, home_suffix: &str| -> anyhow::Result<PathBuf> {
            if let Some(dir) = std::env::var_os(xdg).filter(|dir| !dir.is_empty()) {
                return Ok(PathBuf::from(dir));
            }
            home.clone()
                .map(|home| home.join(home_suffix))
                .ok_or_else(|| anyhow::anyhow!("neither {xdg} nor HOME is set"))
        };
        Ok(Self {
            config_dir: base("XDG_CONFIG_HOME", ".config")?.join("model-router"),
            state_dir: base("XDG_STATE_HOME", ".local/state")?.join("model-router"),
            cache_dir: base("XDG_CACHE_HOME", ".cache")?.join("model-router"),
        })
    }

    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Codex auth material managed by the router's `CLIProxyAPI` child.
    #[must_use]
    pub fn auth_dir(&self) -> PathBuf {
        self.state_dir.join("codex-auth")
    }

    /// Generated `CLIProxyAPI` YAML config (contains the gateway secret).
    #[must_use]
    pub fn upstream_config_file(&self) -> PathBuf {
        self.state_dir.join("cliproxyapi.yaml")
    }

    /// Create-once gateway secret injected on GPT-branch requests.
    #[must_use]
    pub fn secret_file(&self) -> PathBuf {
        self.state_dir.join("gateway-secret")
    }

    /// Single-instance lock for `serve`.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("serve.lock")
    }

    /// Identity of the last managed `CLIProxyAPI` child. Used only to reap a
    /// positively verified orphan left behind by an interrupted router.
    #[must_use]
    pub fn upstream_child_file(&self) -> PathBuf {
        self.state_dir.join("cliproxyapi-child.json")
    }

    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }

    /// Stable service launcher location (bootstrap.sh + binary-version copy);
    /// OS service units exec this, never the ephemeral plugin directory.
    #[must_use]
    pub fn launcher_dir(&self) -> PathBuf {
        self.state_dir.join("launcher")
    }

    /// Cached `CLIProxyAPI` binary for a pinned version.
    #[must_use]
    pub fn upstream_binary(&self, version: &str) -> PathBuf {
        self.cache_dir
            .join("cliproxyapi")
            .join(format!("v{version}"))
            .join("cli-proxy-api")
    }
}

/// Creates a directory (and parents) with mode `0700`.
///
/// # Errors
/// Returns an error when creation or permission tightening fails.
pub fn create_private_dir(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to restrict permissions on {}", path.display()))?;
    }
    Ok(())
}

/// Atomically writes a `0600` file via temp-file + rename in the same
/// directory. Never follows an existing symlink at `path` (the rename
/// replaces it).
///
/// # Errors
/// Returns an error when the parent is missing or any filesystem step fails.
pub fn write_private_atomic(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    let mut temp = tempfile_in(parent)?;
    temp.1
        .write_all(contents)
        .with_context(|| format!("failed to write {}", temp.0.display()))?;
    temp.1
        .sync_all()
        .with_context(|| format!("failed to sync {}", temp.0.display()))?;
    drop(temp.1);
    fs::rename(&temp.0, path)
        .with_context(|| format!("failed to move {} into place", path.display()))?;
    Ok(())
}

fn tempfile_in(dir: &Path) -> anyhow::Result<(PathBuf, fs::File)> {
    for attempt in 0..64u32 {
        let candidate = dir.join(format!(".tmp-{}-{attempt}", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create {}", candidate.display()));
            }
        }
    }
    anyhow::bail!("could not create a temporary file in {}", dir.display())
}

/// Returns the create-once gateway secret, generating it on first use.
///
/// The secret never rotates on its own — a live `CLIProxyAPI` child keeps
/// accepting the credential the router injects.
///
/// # Errors
/// Returns an error when the state directory or secret file cannot be
/// created or read.
pub fn load_or_create_secret(dirs: &Dirs) -> anyhow::Result<String> {
    let path = dirs.secret_file();
    load_or_create_hex_file(dirs, &path, 32)
}

/// Returns the create-once ingress token: the unpredictable URL path prefix
/// (`/t/<token>/...`) the router requires so other local processes cannot
/// spend the user's Codex subscription through the loopback port.
///
/// # Errors
/// Returns an error when the state directory or token file cannot be created
/// or read.
pub fn load_or_create_ingress_token(dirs: &Dirs) -> anyhow::Result<String> {
    let path = dirs.state_dir.join("ingress-token");
    load_or_create_hex_file(dirs, &path, 16)
}

fn load_or_create_hex_file(dirs: &Dirs, path: &Path, bytes: usize) -> anyhow::Result<String> {
    create_private_dir(&dirs.state_dir)?;
    if let Some(existing) = read_nonempty(path)? {
        return Ok(existing);
    }
    // Create-once under concurrency: `create_new` decides a single winner
    // (an atomic rename would let a late loser REPLACE the winner's value,
    // desynchronizing e.g. the served ingress token from the one doctor
    // reports). Losers re-read the winner's value, briefly retrying in case
    // the winner has created but not yet written the file.
    let mut buffer = vec![0u8; bytes];
    getrandom_fill(&mut buffer)?;
    let value = hex_encode(&buffer);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(value.as_bytes())
                .and_then(|()| file.sync_all())
                .with_context(|| format!("failed to write {}", path.display()))?;
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            for _ in 0..100 {
                if let Some(existing) = read_nonempty(path)? {
                    return Ok(existing);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            anyhow::bail!(
                "{} exists but stayed empty; another process may have crashed mid-write — \
                 delete it and retry",
                path.display()
            )
        }
        Err(error) => Err(error).with_context(|| format!("failed to create {}", path.display())),
    }
}

fn read_nonempty(path: &Path) -> anyhow::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(existing) => {
            let value = existing.trim().to_string();
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Lowercase hex encoding (shared by secrets and checksum verification).
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        },
    )
}

fn getrandom_fill(buffer: &mut [u8]) -> anyhow::Result<()> {
    let mut file = fs::File::open("/dev/urandom").context("failed to open /dev/urandom")?;
    std::io::Read::read_exact(&mut file, buffer).context("failed to read /dev/urandom")?;
    Ok(())
}

/// Advisory single-instance lock; held for the lifetime of the returned
/// guard. A second `serve` fails fast instead of fighting over the child.
pub struct InstanceLock {
    _file: fs::File,
    path: PathBuf,
}

impl InstanceLock {
    /// # Errors
    /// Returns an error when the lock is already held by another process or
    /// cannot be created.
    pub fn acquire(dirs: &Dirs) -> anyhow::Result<Self> {
        create_private_dir(&dirs.state_dir)?;
        let path = dirs.lock_file();
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: flock on an owned, open fd.
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                anyhow::bail!(
                    "another model-router instance holds {} — is the service already running?",
                    path.display()
                );
            }
        }
        Ok(Self { _file: file, path })
    }
}

impl std::fmt::Debug for InstanceLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstanceLock")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Copies a Codex login from a legacy `CLIProxyAPI` auth dir into the managed
/// auth dir when the managed dir has none. Returns the imported file name.
///
/// # Errors
/// Returns an error when directory creation or the copy fails.
pub fn import_legacy_auth(dirs: &Dirs) -> anyhow::Result<Option<String>> {
    let auth_dir = dirs.auth_dir();
    create_private_dir(&auth_dir)?;
    if find_codex_auth(&auth_dir).is_some() {
        return Ok(None);
    }
    let legacy_dirs = home_dir()
        .into_iter()
        .flat_map(|home| {
            [
                home.join(".cli-proxy-api-model-router"),
                home.join(".cli-proxy-api"),
            ]
        })
        .collect::<Vec<_>>();
    for legacy in legacy_dirs {
        if let Some(source) = find_codex_auth(&legacy) {
            let name = source
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("codex-imported.json")
                .to_string();
            let contents = fs::read(&source)
                .with_context(|| format!("failed to read {}", source.display()))?;
            write_private_atomic(&auth_dir.join(&name), &contents)?;
            tracing::info!(from = %source.display(), "imported existing Codex login");
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Finds a `codex-*.json` login in a `CLIProxyAPI` auth dir.
#[must_use]
pub fn find_codex_auth(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            let json = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
            json && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("codex-"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dirs(root: &Path) -> Dirs {
        Dirs {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
        }
    }

    #[test]
    fn secret_is_created_once_and_stable() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
        let first = load_or_create_secret(&dirs).unwrap();
        let second = load_or_create_secret(&dirs).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dirs.secret_file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = fs::metadata(&dirs.state_dir).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }
    }

    #[test]
    fn instance_lock_excludes_second_holder() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
        let _held = InstanceLock::acquire(&dirs).unwrap();
        // Same-process flock re-acquisition on a NEW fd still succeeds on
        // some platforms, so exercise exclusion via a child process instead.
        let script = format!(
            "import fcntl,sys\nf=open({:?},'a')\ntry:\n fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB)\nexcept OSError:\n sys.exit(3)\nsys.exit(0)",
            dirs.lock_file().display().to_string()
        );
        let status = std::process::Command::new("python3")
            .arg("-c")
            .arg(script)
            .status();
        if let Ok(status) = status {
            assert_eq!(status.code(), Some(3), "child acquired a held lock");
        }
    }

    #[test]
    fn concurrent_secret_creation_yields_single_value() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let dirs = dirs.clone();
                std::thread::spawn(move || load_or_create_secret(&dirs).unwrap())
            })
            .collect();
        let values: std::collections::HashSet<String> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            values.len(),
            1,
            "every concurrent caller must get one value"
        );
        let persisted = fs::read_to_string(dirs.secret_file()).unwrap();
        assert_eq!(persisted.trim(), values.iter().next().unwrap());
    }

    #[test]
    fn atomic_write_replaces_content() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
        create_private_dir(&dirs.state_dir).unwrap();
        let path = dirs.state_dir.join("file");
        write_private_atomic(&path, b"one").unwrap();
        write_private_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
    }

    #[test]
    fn legacy_auth_import_copies_codex_json() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
        let legacy = root.path().join(".cli-proxy-api-model-router");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("codex-test-user.json"), b"{}").unwrap();
        // Point HOME at the temp root so the legacy scan finds it.
        // (Env mutation is process-global; keep this the only test doing it.)
        unsafe { std::env::set_var("HOME", root.path()) };
        let imported = import_legacy_auth(&dirs).unwrap();
        assert_eq!(imported.as_deref(), Some("codex-test-user.json"));
        assert!(dirs.auth_dir().join("codex-test-user.json").exists());
        // Second call: already present, no re-import.
        assert_eq!(import_legacy_auth(&dirs).unwrap(), None);
    }
}
