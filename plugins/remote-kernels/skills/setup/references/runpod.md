# RunPod setup reference

## API key

`RUNPOD_API_KEY` in `.env.local` — created at
https://docs.runpod.io/get-started/api-keys.

## GPU selection

`[runpod] gpu-type-ids` is a fallback list tried in order — put the
preferred GPU first and one or two acceptable substitutes after it (exact
RunPod type names, e.g. "NVIDIA A100 80GB PCIe"). Size to the workload; the
default RTX 4090 suits most single-GPU experimentation.

## Images: the happy path

The default `runpod/pytorch` image works for most ML and needs zero image
configuration — the pre-SSH orphan guard (a money-safety mechanism, see the
template's `image-start-cmd` comment) applies to it automatically. Extra
tooling is usually better installed via `startup-commands` than a custom
image. Only read the Advanced section below if the user genuinely needs a
custom image.

## Advanced: custom images (`image-start-cmd`)

The template's `image-start-cmd` comment documents the guard mechanics
(what it protects against, the skip conditions, `""` to disable). What the
template can't do is find the value — that's the setup task:

1. Find the custom image's Dockerfile `CMD` — check its Dockerfile or docs,
   run `docker inspect --format '{{.Config.Entrypoint}} {{.Config.Cmd}}'
   <image>` locally if available, or ask the user.
2. Set `[runpod] image-start-cmd` to it.

Edge cases:

- **ENTRYPOINT images**: the wrapper only replaces CMD, so an image whose
  ENTRYPOINT is the workload (CMD just arguments) should leave
  `image-start-cmd` unset.
- **CMD can't be determined confidently**: set `image-start-cmd = ""` and
  warn the user explicitly what that costs: a crash during the first
  minutes of provisioning leaves the pod billing until stopped by hand
  (RunPod console, or `attach()`/`status()` from a later session, which
  can supervise or end it — `start()` always creates a fresh machine).
- `docker-start-cmd` is not a field RunPod's API has, and `start()` rejects
  it. Put the image's start command in `image-start-cmd` (the guard wraps
  it), or set `image-start-cmd = ""` to run the image unwrapped.

## Advanced: Jupyter exposure (`jupyter-access`)

By default ("auto") pods whose config guarantees SSH reach Jupyter through a
local SSH tunnel, but the pod KEEPS its token-protected public proxy mapping
as a fallback for when SSH is slow to come back (e.g. after a resume) — so
the endpoint remains internet-reachable with the token. Users who need
Jupyter physically unreachable from the internet must set
`jupyter-access = "tunnel"`: no public mapping is created at all, at the
cost that a resume whose SSH never returns cannot fall back.
Community-cloud pods use the public proxy (token-protected). Everything
that rides SSH — sync, download, the on-machine watchdog, the pre-SSH orphan
guard, and budget enforcement — is unavailable there, and
`jupyter-access = "tunnel"` is rejected for such a config. Only
`cloud-type = "SECURE"` guarantees SSH, so that is what to set when any of
those matter.
