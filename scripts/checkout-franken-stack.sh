#!/usr/bin/env bash
#
# Materialize ee's sibling path dependencies at the exact revisions recorded
# in franken-stack.lock. Existing checkouts are reused only when their origin,
# HEAD, and clean working tree match the lock. This helper never overwrites or
# removes an existing checkout.

set -euo pipefail

usage() {
  echo "Usage: $0 DESTINATION_ROOT" >&2
  echo "Creates pinned Franken-stack repositories directly under DESTINATION_ROOT." >&2
}

die() {
  echo "franken-stack checkout: $*" >&2
  exit 1
}

if [ "$#" -ne 1 ]; then
  usage
  exit 2
fi

if ! command -v git >/dev/null 2>&1; then
  die "git is required"
fi

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
LOCK_FILE="$REPOSITORY_ROOT/franken-stack.lock"
DESTINATION_ROOT=$1

[ -f "$LOCK_FILE" ] || die "missing lock file: $LOCK_FILE"
[ -n "$DESTINATION_ROOT" ] || die "destination root must not be empty"
mkdir -p "$DESTINATION_ROOT"
DESTINATION_ROOT=$(CDPATH='' cd -- "$DESTINATION_ROOT" && pwd)
[ "$DESTINATION_ROOT" != "/" ] || die "refusing to populate the filesystem root"

is_known_repository() {
  case "$1" in
    asupersync|franken_agent_detection|franken_networkx|frankensearch|frankensqlite|sqlmodel_rust|toon_rust)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

verify_clean_checkout() {
  local repository=$1
  local revision=$2
  local destination=$3
  local expected_url=$4
  local actual_revision
  local actual_url
  local status

  actual_revision=$(git -C "$destination" rev-parse HEAD 2>/dev/null || true)
  [ "$actual_revision" = "$revision" ] || return 1

  actual_url=$(git -C "$destination" remote get-url origin 2>/dev/null || true)
  case "$actual_url" in
    "$expected_url"|"${expected_url%.git}"|"git@github.com:Dicklesworthstone/${repository}.git")
      ;;
    *)
      return 1
      ;;
  esac

  if ! status=$(git -C "$destination" status --porcelain --untracked-files=normal); then
    return 1
  fi
  [ -z "$status" ]
}

checkout_repository() {
  local repository=$1
  local revision=$2
  local destination="$DESTINATION_ROOT/$repository"
  local repository_url="https://github.com/Dicklesworthstone/${repository}.git"
  local marker
  local marker_value
  local actual_revision

  if [ -e "$destination" ]; then
    [ -d "$destination/.git" ] || \
      die "$destination already exists and is not a regular Git checkout"

    if verify_clean_checkout "$repository" "$revision" "$destination" "$repository_url"; then
      echo "franken-stack: reuse ${repository}@${revision}"
      return 0
    fi

    marker="$destination/.git/ee-franken-stack-managed"
    marker_value=""
    [ -f "$marker" ] && marker_value=$(sed -n '1p' "$marker")
    [ "$marker_value" = "${repository}	${revision}" ] || \
      die "$destination does not exactly match ${repository}@${revision}; refusing to modify it"
  else
    mkdir "$destination"
    git -C "$destination" init -q
    git -C "$destination" remote add origin "$repository_url"
    marker="$destination/.git/ee-franken-stack-managed"
    printf '%s\t%s\n' "$repository" "$revision" > "$marker"
  fi

  git -C "$destination" fetch --depth 1 origin "$revision"
  git -C "$destination" -c advice.detachedHead=false checkout --detach FETCH_HEAD

  actual_revision=$(git -C "$destination" rev-parse HEAD)
  [ "$actual_revision" = "$revision" ] || \
    die "$repository resolved to $actual_revision instead of locked revision $revision"
  verify_clean_checkout "$repository" "$revision" "$destination" "$repository_url" || \
    die "$repository checkout is dirty or has unexpected provenance after checkout"

  echo "franken-stack: checked out ${repository}@${revision}"
}

count=0
seen="|"
while IFS=$'\t' read -r repository revision remainder || \
      [ -n "${repository:-}${revision:-}${remainder:-}" ]; do
  case "${repository:-}" in
    ""|\#*)
      continue
      ;;
  esac

  [ -z "${remainder:-}" ] || die "malformed lock row for $repository"
  is_known_repository "$repository" || die "unknown repository in lock: $repository"
  [ "${#revision}" -eq 40 ] || die "revision for $repository is not a full 40-character commit ID"
  case "$revision" in
    *[!0-9a-f]*)
      die "revision for $repository is not lowercase hexadecimal"
      ;;
  esac
  case "$seen" in
    *"|${repository}|"*)
      die "duplicate repository in lock: $repository"
      ;;
  esac

  seen="${seen}${repository}|"
  count=$((count + 1))
  checkout_repository "$repository" "$revision"
done < "$LOCK_FILE"

[ "$count" -eq 7 ] || die "expected 7 locked repositories, found $count"

for required in \
  asupersync \
  franken_agent_detection \
  franken_networkx \
  frankensearch \
  frankensqlite \
  sqlmodel_rust \
  toon_rust
do
  case "$seen" in
    *"|${required}|"*)
      ;;
    *)
      die "required repository missing from lock: $required"
      ;;
  esac
done
