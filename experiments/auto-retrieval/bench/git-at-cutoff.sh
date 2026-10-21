#!/bin/sh
# The bench's git: the alignment-hive repository as it stood at HIVE_LOCAL_CUTOFF, read-only. Each moment gets a
# snapshot clone holding only `main` at the last commit before the cutoff, its working tree checked out there, so
# defaults (HEAD, the working tree, --all) are historical. Objects are shared with the real repository, so a
# commit named by hash is checked: it must be an ancestor of the snapshot's main.
R=/Users/yoav/projects/alignment-hive
G=/usr/bin/git
SNAPS=${GIT_AT_CUTOFF_SNAPSHOTS:-$HOME/.local/share/alignment-hive-auto-retrieval/git-snapshots}
[ -n "$HIVE_LOCAL_CUTOFF" ] || { echo "git: HIVE_LOCAL_CUTOFF is not set" >&2; exit 1; }
while [ "$1" = "-C" ] || [ "$1" = "--no-pager" ] || [ "$1" = "-P" ]; do
  [ "$1" = "-C" ] && shift
  shift
done
base=$($G -C "$R" rev-list -1 --first-parent --before="$HIVE_LOCAL_CUTOFF" main)
[ -n "$base" ] || { echo "git: no history before $HIVE_LOCAL_CUTOFF" >&2; exit 1; }
S=$SNAPS/$base
if [ ! -d "$S" ]; then
  mkdir -p "$SNAPS"
  if mkdir "$S.lock" 2>/dev/null; then
    T=$S.tmp.$$
    $G clone -q --shared --no-checkout --single-branch --no-tags -b main "$R" "$T" &&
      $G -C "$T" remote remove origin &&
      $G -C "$T" update-ref refs/heads/main "$base" &&
      $G -C "$T" checkout -q -f main &&
      $G -C "$T" reflog expire --expire=all --all &&
      mv "$T" "$S"
    rmdir "$S.lock"
    [ -d "$S" ] || { rm -rf "$T"; echo "git: could not prepare the snapshot" >&2; exit 1; }
  else
    while [ -d "$S.lock" ]; do sleep 1; done
  fi
fi
cmd=$1
shift
case "$cmd" in
  log|show|diff|ls-tree|grep|blame|rev-parse) ;;
  *) echo "git: only log, show, diff, ls-tree, grep, blame and rev-parse run here" >&2; exit 1 ;;
esac
# Any hash-like word in the arguments that names a commit must be in the snapshot's history.
for h in $(printf '%s\n' "$@" | grep -oE '[0-9a-f]{7,40}' | sort -u); do
  c=$($G -C "$S" rev-parse --verify -q "$h^{commit}" 2>/dev/null) || continue
  $G -C "$S" merge-base --is-ancestor "$c" "$base" || { echo "git: $h is not in the history as of this moment" >&2; exit 1; }
done
exec $G -C "$S" "$cmd" "$@"
