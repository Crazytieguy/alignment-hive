//! Pinned, checksum-verified acquisition of the `CLIProxyAPI` upstream binary.
//!
//! `ensure_upstream` is idempotent: managed start, `login`, and the
//! `ensure-upstream` subcommand call it in any order, on a clean or warm
//! machine.

use std::path::PathBuf;

use anyhow::Context;
use sha2::Digest;

use crate::state::{Dirs, create_private_dir};
use std::os::unix::fs::PermissionsExt;

/// The exact `CLIProxyAPI` version this router release is validated against.
pub const UPSTREAM_VERSION: &str = "7.3.16";

/// The release archive name for this platform and its sha256, vendored so
/// downloads are verified against the pin rather than trusting the network
/// or the release page.
fn current_target() -> anyhow::Result<(&'static str, &'static str)> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => (
            "darwin_aarch64",
            "b6d478cc16c608abfec9343ee97d3a850403604f12e237952bcfdbad2a03e234",
        ),
        ("macos", "x86_64") => (
            "darwin_amd64",
            "f5dd942200a05687d2132d572ff63c635f3d7b154f34a5d737dadeddd718b828",
        ),
        ("linux", "aarch64") => (
            "linux_aarch64",
            "5af23cdc4c5fc61d260ed6f7cd207fdeee893e174f6866bc5ac6d7d1dddae111",
        ),
        ("linux", "x86_64") => (
            "linux_amd64",
            "64f84d7a08570f8e5310707857bc9edfb032292a9753b5f840da6c8caa325a72",
        ),
        (os, arch) => anyhow::bail!(
            "unsupported platform {os}/{arch}; model-router supports macOS and Linux on x86_64/aarch64"
        ),
    })
}

/// The pinned `CLIProxyAPI` binary's path when it is already cached.
#[must_use]
pub fn cached_upstream(dirs: &Dirs) -> Option<PathBuf> {
    let binary = dirs.upstream_binary(UPSTREAM_VERSION);
    binary.is_file().then_some(binary)
}

/// Ensures the pinned `CLIProxyAPI` binary is cached and executable, returning
/// its path. No-op when already present.
///
/// # Errors
/// Returns an error for unsupported platforms, download failures, checksum
/// mismatches, or archives without the expected binary.
pub async fn ensure_upstream(dirs: &Dirs) -> anyhow::Result<PathBuf> {
    if let Some(binary) = cached_upstream(dirs) {
        return Ok(binary);
    }
    let binary = dirs.upstream_binary(UPSTREAM_VERSION);

    let (target, expected_checksum) = current_target()?;
    let url = format!(
        "https://github.com/router-for-me/CLIProxyAPI/releases/download/v{UPSTREAM_VERSION}/CLIProxyAPI_{UPSTREAM_VERSION}_{target}.tar.gz"
    );

    tracing::info!(%url, "downloading pinned CLIProxyAPI");
    let response = reqwest::get(&url)
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("failed to download {url} — check network access and retry"))?;
    let archive = response
        .bytes()
        .await
        .with_context(|| format!("failed to read download from {url}"))?;

    let actual_checksum = crate::state::hex_encode(&sha2::Sha256::digest(&archive));
    anyhow::ensure!(
        actual_checksum == expected_checksum,
        "checksum mismatch for {url}: expected {expected_checksum}, got {actual_checksum}; refusing to install"
    );

    let parent = binary
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", binary.display()))?
        .to_path_buf();
    create_private_dir(&parent)?;
    let binary_clone = binary.clone();
    tokio::task::spawn_blocking(move || extract_binary(&archive, &binary_clone))
        .await
        .context("extraction task panicked")??;
    tracing::info!(binary = %binary.display(), "CLIProxyAPI installed");
    Ok(binary)
}

/// Extracts the `cli-proxy-api` entry (the archives are flat) to `target`.
fn extract_binary(archive: &[u8], target: &std::path::Path) -> anyhow::Result<()> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().context("invalid release archive")? {
        let mut entry = entry.context("invalid release archive entry")?;
        let path = entry.path().context("invalid entry path")?;
        if path.file_name().and_then(|name| name.to_str()) == Some("cli-proxy-api") {
            let temp = target.with_extension("partial");
            entry
                .unpack(&temp)
                .with_context(|| format!("failed to extract to {}", temp.display()))?;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))
                .context("failed to mark binary executable")?;
            std::fs::rename(&temp, target)
                .with_context(|| format!("failed to move binary to {}", target.display()))?;
            return Ok(());
        }
    }
    anyhow::bail!("release archive does not contain a cli-proxy-api binary")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gzipped tar holding one flat entry.
    fn archive_with(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, payload).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extract_finds_flat_binary() {
        let payload = b"#!/bin/sh\necho fake\n";
        let archive = archive_with("cli-proxy-api", payload);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cli-proxy-api");
        extract_binary(&archive, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), payload);
    }

    #[test]
    fn extract_rejects_archive_without_binary() {
        let archive = archive_with("README.md", b"");
        let dir = tempfile::tempdir().unwrap();
        let error = extract_binary(&archive, &dir.path().join("out")).unwrap_err();
        assert!(error.to_string().contains("does not contain"));
    }
}
