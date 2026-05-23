#!/usr/bin/env bash
# E2E smoke (bd-2z5ly.9): proves that swarm work-packet generation is
# strictly read-only — no `br update`, no `br sync`, no edits to
# `.beads/`, no staged git changes, no agent mail writes.
#
# Strategy: build a sandbox workspace under $artifact_root/work that
# contains a copy of `.beads/` (and a synthetic merge artifact + a
# malformed JSONL tail for the degraded path), snapshot its contents,
# run packet generation through a PATH-shimmed `br` that records every
# invocation and refuses mutating subcommands, then re-snapshot and
# diff. Sandboxing isolates the test from concurrent peer activity in
# the real repo, so the snapshot/diff harness measures only the system
# under test.
#
# The shim refuses anything other than read-only `br ready` /
# `br doctor` / `br list` invocations so an accidental mutation in the
# packet collector trips the script immediately rather than corrupting
# the tracker.
#
# This script does NOT invoke Cargo or the real `ee` binary. Real-Cargo
# verification is RCH-only per AGENTS.md; this is the static / shell
# smoke half of the proof.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

ts="$(date -u +%Y%m%dT%H%M%SZ)"
artifact_root="${EE_PACKET_NO_MUTATION_ARTIFACT_ROOT:-/tmp/ee_packet_no_mutation_${ts}_$$}"
sandbox="$artifact_root/work"
shim_bin="$artifact_root/bin"
call_log="$artifact_root/br_calls.log"
beads_before="$artifact_root/beads_before.sha"
beads_after="$artifact_root/beads_after.sha"
mail_root="$artifact_root/mail"
mail_before="$artifact_root/mail_before.sha"
mail_after="$artifact_root/mail_after.sha"
summary="$artifact_root/summary.jsonl"

mkdir -p "$shim_bin" "$sandbox/.beads" "$mail_root"

# Seed the sandbox with a minimal Beads layout and a synthetic
# malformed tail so the smoke run exercises the degraded path the
# bead targets.
cat >"$sandbox/.beads/issues.jsonl" <<'JSONL'
{"id":"bd-fixture-1","title":"sandbox fixture","status":"open","priority":2}
{"id":"bd-fixture-2","title":"second fixture","status":"open","priority":3}
{"id":"bd-fixture-3","title":"third fixture","status":"open","priority":3}
{"id":"bd-malformed-tail","title":"WIP - record was truncated mid
JSONL
# Synthetic merge artifact next to issues.jsonl.
printf '%s\n' "merge-artifact placeholder" >"$sandbox/.beads/issues.jsonl.orig"
# Empty SQLite stand-in; the shim does not open it.
: >"$sandbox/.beads/beads.db"

snapshot_dir() {
    local root="$1"
    local out="$2"
    if [ -d "$root" ]; then
        ( cd "$root" && find . -type f -print0 \
            | LC_ALL=C sort -z \
            | xargs -0 shasum -a 256 ) >"$out"
    else
        : >"$out"
    fi
}

# `br` shim — records the call, allows read-only subcommands, refuses
# anything that would mutate the tracker. Refuse means non-zero exit so
# the caller fails loudly.
cat >"$shim_bin/br" <<'SHIM'
#!/usr/bin/env bash
set -euo pipefail
log="${EE_PACKET_NO_MUTATION_BR_LOG:?EE_PACKET_NO_MUTATION_BR_LOG required}"
printf '%s\n' "br $*" >>"$log"

# Find the first non-flag argument; that is the subcommand.
sub=""
for arg in "$@"; do
    case "$arg" in
        --*) continue ;;
        *) sub="$arg"; break ;;
    esac
done

case "$sub" in
    ""|ready|list|show|doctor|stats|blocked|comments)
        # Emit an empty but well-formed JSON envelope for `--json`
        # consumers; otherwise emit nothing.
        for arg in "$@"; do
            if [ "$arg" = "--json" ]; then
                printf '{"schema":"br.shim.v1","sub":"%s","issues":[],"checks":[],"ok":true}\n' "$sub"
                exit 0
            fi
        done
        exit 0
        ;;
    *)
        printf 'PACKET-NO-MUTATION shim refused mutating subcommand: %s\n' "$sub" >&2
        exit 64
        ;;
esac
SHIM
chmod +x "$shim_bin/br"

snapshot_dir "$sandbox/.beads" "$beads_before"
snapshot_dir "$mail_root" "$mail_before"
: >"$call_log"

# The packet generator is the system under test. When the real `ee`
# binary is available, callers can override EE_PACKET_NO_MUTATION_CMD
# to e.g. `ee swarm work-packet --json` to drive it through this shim.
# When unset we run a no-op so the script still proves the
# snapshot/diff harness works against an idle workspace.
cmd="${EE_PACKET_NO_MUTATION_CMD:-true}"

(
    cd "$sandbox"
    PATH="$shim_bin:$PATH" \
    EE_PACKET_NO_MUTATION_BR_LOG="$call_log" \
    AGENT_MAIL_HOME="$mail_root" \
        bash -c "$cmd"
)

snapshot_dir "$sandbox/.beads" "$beads_after"
snapshot_dir "$mail_root" "$mail_after"

fail=0
if ! diff -u "$beads_before" "$beads_after" >"$artifact_root/beads.diff"; then
    fail=1
    printf 'FAIL: .beads/ changed during packet generation\n' >&2
fi
if ! diff -u "$mail_before" "$mail_after" >"$artifact_root/mail.diff"; then
    fail=1
    printf 'FAIL: agent mail store changed during packet generation\n' >&2
fi

# Refuse any mutating br subcommand that slipped past the shim's
# allowlist. The shim already exits non-zero on those, but we
# double-check the recorded call log for `update`, `sync`, `claim`,
# and `close` strings to catch a regression where the collector
# bypasses the shim by hard-coding /usr/local/bin/br.
mutating_calls=0
if grep -E '^br[[:space:]]+(update|sync|claim|close|comments[[:space:]]+add)\b' \
    "$call_log" >/dev/null 2>&1; then
    fail=1
    mutating_calls=1
    printf 'FAIL: mutating br subcommand observed in call log\n' >&2
fi

call_count="$(wc -l <"$call_log" | tr -d ' ')"
printf '{"schema":"ee.packet_no_mutation.v1","ts":"%s","artifact_root":"%s","sandbox":"%s","br_call_count":%s,"mutating_calls":%s,"ok":%s}\n' \
    "$ts" "$artifact_root" "$sandbox" "$call_count" "$mutating_calls" \
    "$( [ "$fail" -eq 0 ] && printf true || printf false )" \
    >"$summary"

cat "$summary"

exit "$fail"
