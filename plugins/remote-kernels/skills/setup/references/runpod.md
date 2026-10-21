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

## Network volumes

A network volume (`network-volume-id`) outlives every pod, and several pods
can mount it at once: the place for data worth keeping across pods (model
downloads, HF/uv caches, datasets, checkpoints), pointed at through `[env]`
(e.g. `HF_HOME`). Keep the project itself off it by setting `workdir` to a
directory outside the mount. Otherwise `sync()` mirrors the project onto the
volume, deleting everything there the project doesn't have (gitignored
paths excepted), and venvs and imports run on network storage, many times
slower than the pod's own disk.

For a volume already in use (the session-start row "files sync onto your
RunPod network volume"), keep `volume-mount-path` where it is so the data's
paths don't move, and set `workdir` to a directory on the pod's disk such as
`/root/work`. Code that reaches project files by absolute paths under the
mount needs updating. The pod's disk is wiped on stop, so the tree and venv
come back with the next sync and install. A user who would rather keep the
project on the volume sets `workdir` to the mount path explicitly, which
also clears the row.

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
  (a later session can `stop(instance=...)` or `terminate(instance=...)`
  it, or `attach()`/`status()` to supervise it — `start()` always creates a
  fresh machine).
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
Community-cloud pods use the public proxy (token-protected). Sync,
download, automatic cleanup, and budget enforcement require SSH; only
`cloud-type = "SECURE"` guarantees it, so set that when those features
matter. (`jupyter-access = "tunnel"` is rejected for a config without that
guarantee.)
