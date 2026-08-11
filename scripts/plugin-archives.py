#!/usr/bin/env python3
"""Build and publish the platform-specific plugin archives.

Plugins that ship a binary (any `plugins/*/binary-version`) are also published
as `archive` marketplace entries: one zip per platform, with that platform's
released binary bundled inside at bin/<plugin>-<target>.tar.xz. Plugin and
binary then install as one artifact, so a session can never run a new plugin
against an old binary.

The zips live at fixed urls, on one rolling release. Claude Code downloads an
archive-sourced plugin on every update pass and reads the version out of the
downloaded zip, so the url carries no version and `marketplace.json` never
changes: publishing replaces the assets in place. At any instant a url serves a
coherent plugin+binary pair that is either current or one release behind, so a
fresh install during a release gets a working older version rather than a 404.

Rolling back is `git revert` + land: the build is byte-deterministic (contents
and modes from `git ls-files -s`, sorted entries, fixed timestamps, stored
uncompressed), so CI reproduces the previous zips exactly and puts them back.
Determinism also lets publish skip assets that are already identical.

Commands:
  build     build the zips into dist/plugin-archives/ (downloads the binaries)
  publish   build, then upload, replacing what is there (CI).

Naming a plugin means "its binary release exists": a missing one is then an
error. Unnamed, every plugin is published and a missing binary release is
skipped, because a push that bumps binary-version races the release building it.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

REPO = "Crazytieguy/alignment-hive"

# One rolling release holds every plugin's archives. No digits, so it cannot
# match the cargo-dist release workflows' tag filters.
ARCHIVE_TAG = "plugin-archives"

RELEASE_NOTES = (
    "Platform-specific plugin archives, referenced by the `archive` marketplace "
    "entries. The urls are fixed — each release replaces these assets in place, "
    "and the version lives in the `plugin.json` inside each zip."
)

# 1980-01-01, the earliest timestamp the zip format can represent.
ZIP_EPOCH = (1980, 1, 1, 0, 0, 0)


class MissingRelease(Exception):
    """The binary release a plugin pins has not been published."""


def run(*args: str, cwd: Path | None = None) -> str:
    result = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(
            f"error: `{' '.join(args)}` exited {result.returncode}\n"
            f"{result.stderr.strip() or result.stdout.strip()}"
        )
    return result.stdout


def repo_root() -> Path:
    return Path(run("git", "rev-parse", "--show-toplevel").strip())


def binary_plugins(root: Path) -> list[str]:
    """Every plugin that ships a binary, i.e. every one with a binary-version."""
    return sorted(p.parent.name for p in root.glob("plugins/*/binary-version"))


def targets(root: Path) -> list[str]:
    """The cargo-dist target matrix — the authority on which binaries exist."""
    return tomllib.loads((root / "dist-workspace.toml").read_text())["dist"]["targets"]


def binary_version(root: Path, plugin: str) -> str:
    return (root / "plugins" / plugin / "binary-version").read_text().strip()


def download_url(tag: str, name: str) -> str:
    return f"https://github.com/{REPO}/releases/download/{tag}/{name}"


# --- building ----------------------------------------------------------------


def fetch(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=120) as response:
        return response.read()


def binary_asset(root: Path, plugin: str, version: str, target: str) -> bytes:
    """The cargo-dist .tar.xz for one target, checked against its sidecar.

    The sidecar is fetched even on a cache hit. Release assets are mutable and
    Rust builds are not reproducible, so re-running a binary release for an
    existing tag replaces the bytes under the same version — without this, a
    warm cache would keep bundling the superseded binary forever.
    """
    name = f"{plugin}-{target}.tar.xz"
    tag = f"{plugin}-v{version}"
    cache = root / "dist" / "plugin-archives" / ".cache" / tag / name

    try:
        expected = fetch(download_url(tag, f"{name}.sha256")).decode().split()[0]
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise MissingRelease(
                f"{plugin} binary v{version} has no release asset {name}"
            ) from error
        raise

    if cache.exists():
        data = cache.read_bytes()
        if hashlib.sha256(data).hexdigest() == expected:
            return data
        print(f"warning: cached {name} is stale, re-downloading", file=sys.stderr)

    data = fetch(download_url(tag, name))
    actual = hashlib.sha256(data).hexdigest()
    if actual != expected:
        sys.exit(f"error: {name} digest {actual} != published {expected}")

    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.write_bytes(data)
    return data


def plugin_files(root: Path, plugin: str) -> list[tuple[str, int, bytes]]:
    """(path relative to the plugin dir, unix mode, contents), from git.

    git — not a filesystem walk — is the authority here: it carries the
    executable bit the setup skills depend on (they run bootstrap.sh directly),
    and it excludes untracked junk that would otherwise vary between machines.
    """
    prefix = f"plugins/{plugin}/"
    files = []
    for line in run("git", "ls-files", "-s", "--", prefix, cwd=root).splitlines():
        meta, path = line.split("\t", 1)
        git_mode = meta.split()[0]
        if git_mode not in ("100644", "100755"):
            sys.exit(f"error: {path} has unsupported git mode {git_mode}")
        mode = 0o755 if git_mode == "100755" else 0o644
        files.append((path[len(prefix):], mode, (root / path).read_bytes()))
    if not files:
        sys.exit(f"error: no tracked files under {prefix}")
    return sorted(files)


def write_entry(archive: zipfile.ZipFile, name: str, mode: int, data: bytes) -> None:
    info = zipfile.ZipInfo(name, date_time=ZIP_EPOCH)
    info.create_system = 3  # unix, so the mode bits below are honoured
    info.external_attr = mode << 16
    archive.writestr(info, data)


def build_zip(files: list[tuple[str, int, bytes]], bundle: str, binary: bytes) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_STORED) as archive:
        for relative, mode, data in files:
            write_entry(archive, relative, mode, data)
        write_entry(archive, bundle, 0o644, binary)
    return buffer.getvalue()


def build(root: Path, plugins: list[str], strict: bool) -> list[dict]:
    out_dir = root / "dist" / "plugin-archives"
    built = []

    for plugin in plugins:
        binver = binary_version(root, plugin)
        files = plugin_files(root, plugin)
        archives = []
        try:
            for target in targets(root):
                # Must match the name bootstrap.sh looks up for its own target:
                # a bin/ that holds only some other target's bundle is what tells
                # it the wrong variant is installed.
                bundle = f"bin/{plugin}-{target}.tar.xz"
                data = build_zip(files, bundle, binary_asset(root, plugin, binver, target))
                path = out_dir / f"{plugin}-{target}.zip"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
                archives.append(
                    {
                        "name": path.name,
                        "path": path,
                        "sha256": hashlib.sha256(data).hexdigest(),
                    }
                )
        except MissingRelease as error:
            if strict:
                sys.exit(f"error: {error} — release the binary first")
            # A push that bumps binary-version races the release that builds it;
            # the release workflow calls publish again once the binary exists.
            print(f"warning: skipping {plugin} archives — {error}", file=sys.stderr)
            continue

        built.append({"plugin": plugin, "archives": archives})

    for entry in built:
        for archive in entry["archives"]:
            size = archive["path"].stat().st_size
            print(f"{archive['sha256']}  {size / 1e6:5.2f} MB  {archive['name']}")
    return built


# --- publishing --------------------------------------------------------------


def published_digests() -> dict[str, str] | None:
    """{asset name: sha256} for the rolling release, or None if it is absent."""
    result = subprocess.run(
        ["gh", "api", f"repos/{REPO}/releases/tags/{ARCHIVE_TAG}"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        # Only a genuine 404 means "not created yet". Reporting a network blip
        # as absent would send us into `gh release create`, which then fails
        # with a misleading "already exists".
        if "HTTP 404" not in result.stderr:
            sys.exit(f"error: could not read release {ARCHIVE_TAG}\n{result.stderr.strip()}")
        return None
    return {
        asset["name"]: (asset.get("digest") or "").removeprefix("sha256:")
        for asset in json.loads(result.stdout)["assets"]
    }


def publish(built: list[dict]) -> int:
    digests = published_digests()

    if digests is None:
        print(f"Creating release {ARCHIVE_TAG}")
        run(
            "gh", "release", "create", ARCHIVE_TAG,
            "--title", "Plugin archives (rolling)",
            "--notes", RELEASE_NOTES,
        )
        digests = {}

    for entry in built:
        for archive in entry["archives"]:
            if digests.get(archive["name"]) == archive["sha256"]:
                print(f"{archive['name']} already published and identical")
                continue
            print(f"Uploading {archive['name']}")
            run("gh", "release", "upload", ARCHIVE_TAG, str(archive["path"]), "--clobber")

    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Build and publish the platform-specific plugin archives."
    )
    parser.add_argument("command", choices=("build", "publish"))
    parser.add_argument(
        "plugins", nargs="*", help="limit to these plugins (default: all binary plugins)"
    )
    args = parser.parse_args()

    root = repo_root()
    known = binary_plugins(root)
    unknown = set(args.plugins) - set(known)
    if unknown:
        parser.error(f"not binary-shipping plugins: {', '.join(sorted(unknown))}")

    built = build(root, args.plugins or known, strict=bool(args.plugins))
    if args.command == "publish":
        return publish(built)
    return 0


if __name__ == "__main__":
    sys.exit(main())
