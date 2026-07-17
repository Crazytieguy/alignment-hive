# vast.ai setup reference

## API key (and the 2FA constraint)

`VAST_API_KEY` in `.env.local` — a plain console key from
https://cloud.vast.ai/manage-keys/ (plain keys never expire).

The plugin does not support 2FA-enabled vast accounts: with 2FA on, API
writes (instance creation) fail with a 401 mentioning Two Factor
Authentication. If the user hits that, have them disable 2FA on the vast
account (cloud.vast.ai → Account → Security) — warn that the vast UI makes
it easy to enable 2FA accidentally. A user who insists on keeping 2FA can
mint a session key by hand (`POST /api/v0/tfa/` with the console key as
Bearer and a fresh TOTP code; store the returned `session_key`), but it
expires after ~1-2 days and needs re-minting each time — say so before they
choose.

## Machines are ephemeral

Stop is unreliable on vast: the GPU can be re-rented to someone else while
stopped (resume may hang forever), and storage keeps billing until the
instance is terminated. Prefer `cleanup = "terminate"` and make the
data-persistence plan (see the skill) load-bearing — anything not brought
back is lost.

## Host selection

Two modes, both worth explaining to the user:

- **Claude picks**: `search_vast_offers()` returns a table of hosts plus
  picking advice; Claude ranks a shortlist and passes it to
  `start(vast_offers=[...])`. The user can just say "pick a host for me" or
  "find me a good deal" in any session.
- **Automatic**: plain `start()` takes the cheapest offers that pass the
  configured filters — zero friction, but cheapest-first can land on slower
  hosts.

`selection-guidance` is appended to the advice Claude sees on every search,
so it expresses HOW to pick when a search happens (price, locality,
bandwidth, host quality) — not WHETHER to search. If the user wants
automatic selection by default, that's a workflow preference: record it in
the project's CLAUDE.md or a memory, not in `selection-guidance`.

Ask what the user tends to care about when picking GPUs and write that into
`selection-guidance`.

## VM mode (Docker-in-Docker)

Set `vm = true` for anything that needs Docker inside (e.g. Inspect's
sandboxed evals) — vast bans Docker-in-Docker on container instances. VM
images ship Docker and CUDA preinstalled; `onstart` installs anything else.
For Docker-dependent workloads add a guard onstart line:
`docker info >/dev/null 2>&1 || (curl -fsSL https://get.docker.com | sh)`.

## Advanced: images, filters, vendor traps

- Container default is `vastai/base-image`; VMs default to
  `vastai/kvm:ubuntu_terminal`. Two vendor traps the runtime handles
  automatically (relevant only when creating VMs by hand): the image must
  be registry-qualified (`docker.io/vastai/kvm:...`) or vast silently
  creates a container instead of a VM, and vast's SSH proxy can't reach
  VMs — only direct-port hosts work.
- The baseline search filters are documented in the `[vast]` template
  section; every one can be overridden via `[vast.query]`, and per-call
  tool arguments override both.
