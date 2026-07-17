# Kubernetes setup reference

## Credentials

No API key — the plugin uses the kubeconfig. If the user works across
several clusters, set `[kubernetes] context` (and `namespace`) explicitly so
a switched current-context can't land pods on the wrong cluster.

## Pod template (required)

The lab owns a pod YAML with the cluster specifics: GPU resources,
tolerations, volumes, and the Kueue `queue-name` label. Point
`[kubernetes] pod-template` at it (ask the lab for one if it doesn't exist).

Template contract: the workload container's image provides `sh`, `tar`, and
Python with `jupyter-server` + `ipykernel`; the pod keeps itself alive
(e.g. `command: ["sleep", "infinity"]`).

- If the template lists multiple containers, set `container-name` to the
  workload container (the one that gets env vars and the Jupyter token and
  runs kernels) — otherwise the FIRST container is assumed.
- `start(priority="high")` sets the Kueue workload-priority label
  (configurable via `priority-label`).

Docs: https://kueue.sigs.k8s.io/docs/tasks/run/plain_pods/ and
https://kubernetes.io/docs/concepts/workloads/pods/

## Pod lifetime

`max-lifetime-secs` ships disabled — the template comment documents the
mechanics and tradeoffs. Check whether the lab's pod template sets its own
`activeDeadlineSeconds`; if neither bounds the pod, say so explicitly and
tie the decision to the data-persistence discussion (a bound that fires
kills the pod mid-run; no bound means forgotten pods run until deleted by
hand).

## Data persistence

Pods have no stop/resume — terminate loses everything not persisted. PVCs in
the pod template are the natural home for durable data; otherwise `download`
results before pods go away.
