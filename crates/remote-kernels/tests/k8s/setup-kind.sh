#!/bin/bash
set -euo pipefail

# Stand up the local kind cluster used by tests/k8s_e2e.rs:
# - cluster "remote-kernels-e2e"
# - fake nvidia.com/gpu capacity patched onto the node (the scheduler treats
#   extended resources as opaque integers, so GPU-requesting pods schedule
#   with no hardware behind them)
# - Kueue installed with a default flavor/queue, so queue-labeled pods are
#   gated and admitted like on a real ML cluster
#
# Usage: tests/k8s/setup-kind.sh [--delete]

CLUSTER=remote-kernels-e2e
# ≥ v0.16 — plain-Pod integration (which we rely on) is default-on from there.
KUEUE_VERSION=v0.17.2
IMAGE=quay.io/jupyter/base-notebook:latest

if [ "${1:-}" = "--delete" ]; then
  kind delete cluster --name "$CLUSTER"
  exit 0
fi

if ! kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
  kind create cluster --name "$CLUSTER" --wait 120s
fi
kubectl config use-context "kind-$CLUSTER" >/dev/null

# Fake GPU capacity on every node.
for node in $(kubectl get nodes -o name); do
  kubectl patch "$node" --subresource=status --type=merge \
    -p '{"status":{"capacity":{"nvidia.com/gpu":"8"},"allocatable":{"nvidia.com/gpu":"8"}}}'
done

# Pre-pull the notebook image into the cluster (idempotent, big first pull).
if ! docker exec "$CLUSTER-control-plane" crictl images 2>/dev/null | grep -q jupyter/base-notebook; then
  docker pull "$IMAGE"
  kind load docker-image "$IMAGE" --name "$CLUSTER"
fi

# Kueue + a default single-flavor queue (reapplied when the version changes).
INSTALLED=$(kubectl -n kueue-system get deploy kueue-controller-manager \
  -o jsonpath='{.spec.template.spec.containers[0].image}' 2>/dev/null | grep -o 'v[0-9.]*$' || true)
if [ "$INSTALLED" != "$KUEUE_VERSION" ]; then
  kubectl apply --server-side --force-conflicts -f \
    "https://github.com/kubernetes-sigs/kueue/releases/download/$KUEUE_VERSION/manifests.yaml"
fi
kubectl -n kueue-system rollout status deploy/kueue-controller-manager --timeout=180s

# Kueue webhooks need a moment after rollout; retry queue creation.
QUEUE_APPLIED=0
for _ in $(seq 1 10); do
  if kubectl apply -f - <<'EOF' 2>/dev/null; then QUEUE_APPLIED=1; break; fi
apiVersion: kueue.x-k8s.io/v1beta1
kind: ResourceFlavor
metadata:
  name: default-flavor
---
apiVersion: kueue.x-k8s.io/v1beta1
kind: ClusterQueue
metadata:
  name: main-queue
spec:
  namespaceSelector: {}
  resourceGroups:
    - coveredResources: ["cpu", "memory", "nvidia.com/gpu"]
      flavors:
        - name: default-flavor
          resources:
            - name: cpu
              nominalQuota: 16
            - name: memory
              nominalQuota: 32Gi
            - name: nvidia.com/gpu
              nominalQuota: 8
---
apiVersion: kueue.x-k8s.io/v1beta1
kind: LocalQueue
metadata:
  name: main
  namespace: default
spec:
  clusterQueue: main-queue
---
apiVersion: kueue.x-k8s.io/v1beta1
kind: WorkloadPriorityClass
metadata:
  name: high
value: 1000
EOF
  sleep 5
done
if [ "$QUEUE_APPLIED" != 1 ]; then
  echo "ERROR: failed to create Kueue queue objects after 10 attempts" >&2
  exit 1
fi

echo "kind cluster '$CLUSTER' ready (context kind-$CLUSTER)"
