//! Pinned, checksum-verified acquisition of the `CLIProxyAPI` upstream binary.
//!
//! `ensure_upstream` is the idempotent primitive the plan requires: called
//! by managed start, `login`, `doctor --fix`-style repair, and the setup
//! skill, in any order, on a clean or warm machine.

use std::path::PathBuf;

use anyhow::Context;
use sha2::Digest;

use crate::state::{Dirs, create_private_dir};

/// The exact `CLIProxyAPI` version this router release is validated against.
pub const UPSTREAM_VERSION: &str = "7.2.92";

/// sha256 of each release archive, vendored so downloads are verified
/// against the pin rather than trusting the network or the release page.
const CHECKSUMS: &[(&str, &str)] = &[
    (
        "darwin_aarch64",
        "fc9d2020c0961d097e0b8082d043006af534df261537a6a73756c6d60b4d6524",
    ),
    (
        "darwin_amd64",
        "7eb101925741b4080fe2fae3a7d630f41445c4fd5bf8174672dcaf1f637212b9",
    ),
    (
        "linux_aarch64",
        "4cf0c1febc6a62e77181f5fa3fd334070a0d859d8824d51f7854aa50d52b98b1",
    ),
    (
        "linux_amd64",
        "18ef0621b42bd12fd040789affa7d596343fea70359f59719c8c82d4f9ac0886",
    ),
];

fn current_target() -> anyhow::Result<&'static str> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin_aarch64",
        ("macos", "x86_64") => "darwin_amd64",
        ("linux", "aarch64") => "linux_aarch64",
        ("linux", "x86_64") => "linux_amd64",
        (os, arch) => anyhow::bail!(
            "unsupported platform {os}/{arch}; model-router supports macOS and Linux on x86_64/aarch64"
        ),
    };
    Ok(target)
}

/// Ensures the pinned `CLIProxyAPI` binary is cached and executable, returning
/// its path. No-op when already present.
///
/// # Errors
/// Returns an error for unsupported platforms, download failures, checksum
/// mismatches, or archives without the expected binary.
pub async fn ensure_upstream(dirs: &Dirs) -> anyhow::Result<PathBuf> {
    let binary = dirs.upstream_binary(UPSTREAM_VERSION);
    if binary.is_file() {
        return Ok(binary);
    }

    let target = current_target()?;
    let expected_checksum = CHECKSUMS
        .iter()
        .find(|(name, _)| *name == target)
        .map(|(_, checksum)| *checksum)
        .ok_or_else(|| anyhow::anyhow!("no vendored checksum for target {target}"))?;
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
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))
                    .context("failed to mark binary executable")?;
            }
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

    #[test]
    fn every_supported_target_has_a_checksum() {
        for target in [
            "darwin_aarch64",
            "darwin_amd64",
            "linux_aarch64",
            "linux_amd64",
        ] {
            assert!(CHECKSUMS.iter().any(|(name, _)| *name == target));
        }
    }

    #[test]
    fn extract_finds_flat_binary() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let payload = b"#!/bin/sh\necho fake\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "cli-proxy-api", payload.as_slice())
            .unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cli-proxy-api");
        extract_binary(&archive, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), payload);
    }

    #[test]
    fn extract_rejects_archive_without_binary() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "README.md", [].as_slice())
            .unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let error = extract_binary(&archive, &dir.path().join("out")).unwrap_err();
        assert!(error.to_string().contains("does not contain"));
    }
}
