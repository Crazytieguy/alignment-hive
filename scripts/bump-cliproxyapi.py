#!/usr/bin/env python3
"""Move model-router's CLIProxyAPI pin to a release that has aged.

The managed upstream holds the Codex OAuth credential and sees every GPT
request, and CLIProxyAPI ships about a release a day. So the pin never follows
"latest": by default it moves to the newest release published at least
--min-age-days ago, which gives the wider ecosystem a week to notice and pull a
bad release before it is ever downloaded here.

For the chosen version the script downloads the four archives model-router
supports, checks each sha256 against the release's own checksums.txt, and
rewrites UPSTREAM_VERSION plus the vendored hashes in
crates/model-router/src/acquire.rs. It writes nothing when the pin is already
at or past the target.

Naming a --version younger than the minimum age is refused unless
--allow-fresh is passed; the workflow never auto-merges such a bump.

Outputs (key=value lines, appended to $GITHUB_OUTPUT when set):
  changed    true|false
  version    the version now pinned
  previous   the version pinned before
  published  the release's publish time (UTC, ISO 8601)
  fresh      true when the release is younger than the minimum age
"""

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import sys
import urllib.request
from pathlib import Path

REPO = "router-for-me/CLIProxyAPI"
ACQUIRE = Path(__file__).resolve().parent.parent / "crates/model-router/src/acquire.rs"
# The targets current_target() in acquire.rs knows about.
TARGETS = ["darwin_aarch64", "darwin_amd64", "linux_aarch64", "linux_amd64"]
VERSION_RE = re.compile(r'(pub const UPSTREAM_VERSION: &str = ")([^"]+)(")')


def get(url: str) -> bytes:
    headers = {"User-Agent": "alignment-hive-cliproxy-bump"}
    if token := os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN"):
        if url.startswith("https://api.github.com/"):
            headers["Authorization"] = f"Bearer {token}"
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=120) as r:
        return r.read()


def semver(v: str) -> tuple[int, ...]:
    return tuple(int(p) for p in v.split("."))


def releases() -> list[dict]:
    out = []
    for page in range(1, 4):
        batch = json.loads(get(f"https://api.github.com/repos/{REPO}/releases?per_page=100&page={page}"))
        out += [r for r in batch if not r["draft"] and not r["prerelease"] and re.fullmatch(r"v\d+\.\d+\.\d+", r["tag_name"])]
        if len(batch) < 100:
            break
    return out


def published(r: dict) -> dt.datetime:
    return dt.datetime.fromisoformat(r["published_at"].replace("Z", "+00:00"))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--min-age-days", type=float, default=7.0)
    ap.add_argument("--version", help="pin this exact version (no leading v) instead of the newest aged one")
    ap.add_argument("--allow-fresh", action="store_true", help="accept a --version younger than the minimum age")
    ap.add_argument("--dry-run", action="store_true", help="choose and verify, but do not rewrite acquire.rs")
    args = ap.parse_args()

    source = ACQUIRE.read_text()
    current = VERSION_RE.search(source).group(2)
    now = dt.datetime.now(dt.UTC)
    cutoff = now - dt.timedelta(days=args.min_age_days)
    by_version = {r["tag_name"][1:]: r for r in releases()}

    if args.version:
        release = by_version.get(args.version)
        if release is None:
            sys.exit(f"CLIProxyAPI v{args.version} is not a published release")
        if published(release) > cutoff and not args.allow_fresh:
            sys.exit(f"v{args.version} was published {published(release):%Y-%m-%d %H:%M} UTC, under {args.min_age_days:g} days ago; pass --allow-fresh to pin it anyway")
    else:
        aged = [v for v, r in by_version.items() if published(r) <= cutoff]
        if not aged:
            sys.exit(f"no CLIProxyAPI release is at least {args.min_age_days:g} days old")
        release = by_version[max(aged, key=semver)]
    target = release["tag_name"][1:]
    fresh = published(release) > cutoff

    outputs = {"previous": current, "published": release["published_at"], "fresh": str(fresh).lower()}
    if semver(target) <= semver(current):
        print(f"pin v{current} is at or past v{target}; nothing to do")
        outputs |= {"changed": "false", "version": current}
    else:
        assets = {a["name"]: a["browser_download_url"] for a in release["assets"]}
        if "checksums.txt" not in assets:
            sys.exit(f"v{target} publishes no checksums.txt; refusing to vendor unverified hashes")
        listed = {}
        for line in get(assets["checksums.txt"]).decode().splitlines():
            if len(parts := line.split()) == 2:
                listed[parts[1].lstrip("*")] = parts[0].lower()
        for t in TARGETS:
            name = f"CLIProxyAPI_{target}_{t}.tar.gz"
            if name not in assets or name not in listed:
                sys.exit(f"v{target} is missing {name} or its checksum")
            digest = hashlib.sha256(get(assets[name])).hexdigest()
            if digest != listed[name]:
                sys.exit(f"{name}: downloaded sha256 {digest} != checksums.txt {listed[name]}; refusing")
            source, n = re.subn(rf'("{t}",\s*")[0-9a-f]{{64}}(")', rf"\g<1>{digest}\g<2>", source)
            if n != 1:
                sys.exit(f"expected exactly one {t} hash in {ACQUIRE}, found {n}")
            print(f"verified {name} {digest}")
        source = VERSION_RE.sub(rf"\g<1>{target}\g<3>", source)
        if not args.dry_run:
            ACQUIRE.write_text(source)
        print(f"pin v{current} -> v{target} (published {release['published_at']}{', FRESH' if fresh else ''})")
        outputs |= {"changed": "false" if args.dry_run else "true", "version": target}

    lines = "".join(f"{k}={v}\n" for k, v in outputs.items())
    if path := os.environ.get("GITHUB_OUTPUT"):
        with open(path, "a") as f:
            f.write(lines)
    print(lines, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
