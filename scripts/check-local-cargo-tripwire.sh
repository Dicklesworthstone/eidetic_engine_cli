#!/usr/bin/env bash
# bd-1h8ji.2 — Local Cargo tripwire / RCH hook-bypass detector.
#
# Classifies a candidate cargo invocation against the bd-1h8ji.1 verifier
# contract: direct `cargo build/check/test/bench/clippy` in this repo
# fails-closed unless wrapped through the repo verifier wrapper or an
# explicitly remote-required `rch exec -- ... cargo ...` command. Also
# detects already-running local `cargo`/`rustc` processes that are
# writing into Mac-local USB target dirs without an RCH wrapper visible
# in their parent chain — the exact failure the bead body cites where
# a direct `cargo bench` with `RCH_REQUIRE_REMOTE=1` set still started
# local Darwin work.
#
# This is the READ-ONLY DETECTION half of bd-1h8ji.2. It never deletes,
# kills, or otherwise mutates state. The active PreToolUse hook that
# refuses to spawn the underlying process before it forks is the
# explicit follow-up child slice.
#
# Usage:
#   scripts/check-local-cargo-tripwire.sh --cmd '<command-line>' [--json]
#   scripts/check-local-cargo-tripwire.sh --probe-processes [--ps-file <fixture>] [--package-cache-pids <csv>] [--worktree-file <fixture>] [--tmux-panes-file <fixture>] [--json]
#   scripts/check-local-cargo-tripwire.sh --self-test
#
# Exit codes: 0 = allowed/clean, 1 = bypass detected, 2 = usage error.

set -eu

REPORT_SCHEMA="ee.rch_local_cargo_tripwire.v1"
REQUIRED_REMOTE_WRAPPER="scripts/rch_verify.sh -- <cargo command>"
JSON_OUTPUT=false
SELF_TEST=false
MODE="cmd_classify"
CMD=""
CMD_PROVIDED=false
PS_FIXTURE=""
PACKAGE_CACHE_PIDS_FIXTURE=""
WORKTREE_FIXTURE=""
TMUX_PANES_FIXTURE=""
TMUX_PANES_TEXT_FIXTURE=""
PROCESS_STAT_TEXT_FIXTURE=""

usage() {
    sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --json) JSON_OUTPUT=true; shift ;;
        --self-test) SELF_TEST=true; shift ;;
        --probe-processes) MODE="probe_processes"; shift ;;
        --ps-file)
            shift
            if [ $# -eq 0 ]; then
                printf -- '--ps-file requires a value\n' >&2
                usage >&2
                exit 2
            fi
            PS_FIXTURE="$1"
            shift
            ;;
        --package-cache-pids)
            shift
            if [ $# -eq 0 ]; then
                printf -- '--package-cache-pids requires a value\n' >&2
                usage >&2
                exit 2
            fi
            PACKAGE_CACHE_PIDS_FIXTURE="$1"
            shift
            ;;
        --worktree-file)
            shift
            if [ $# -eq 0 ]; then
                printf -- '--worktree-file requires a value\n' >&2
                usage >&2
                exit 2
            fi
            WORKTREE_FIXTURE="$1"
            shift
            ;;
        --tmux-panes-file)
            shift
            if [ $# -eq 0 ]; then
                printf -- '--tmux-panes-file requires a value\n' >&2
                usage >&2
                exit 2
            fi
            TMUX_PANES_FIXTURE="$1"
            shift
            ;;
        --cmd)
            shift
            if [ $# -eq 0 ]; then
                printf -- '--cmd requires a value\n' >&2
                usage >&2
                exit 2
            fi
            CMD="$1"
            CMD_PROVIDED=true
            shift
            ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) printf 'unknown flag: %s\n' "$1" >&2; usage >&2; exit 2 ;;
        *) printf 'unexpected positional arg: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

# Cargo subcommands that trigger compilation. Plain `cargo metadata`,
# `cargo locate-project`, etc. are NOT compile commands and don't trip
# the wire.
FORBIDDEN_CARGO_SUBCOMMANDS="build check test bench clippy doc run install rustc fix"

# Repo-specific path tokens that indicate the cargo command is
# operating on this checkout (not some sibling crate). When detection
# walks live processes, we use these to bound false positives to the
# eidetic_engine_cli tree.
REPO_PATH_HINTS="eidetic_engine_cli /data/projects/eidetic_engine_cli /Users/jemanuel/projects/eidetic_engine_cli"
READ_ONLY_CARGO_SUBCOMMANDS="metadata locate-project pkgid tree"

classify_command() {
    # Returns a single line "<allowed>\t<reason>\t<subcommand>\t<detail>"
    # where allowed is "allowed" or "denied". The detail field carries
    # a short example/anomaly fingerprint for human/JSON output.
    local cmd="$1"
    local subcommand=""
    local detail=""

    # Empty command can't be a tripwire violation.
    if [ -z "$cmd" ]; then
        printf 'allowed\tempty command\t-\t-\n'
        return
    fi

    # Shell command substitution runs before the outer command receives
    # its arguments. A tracker command such as `br comments add --message
    # "$(cargo test ...)"` can therefore start local Cargo before Beads
    # sees the comment text. Deny Rust verifier commands in substitution
    # forms before applying the RCH-wrapper allowlist below.
    # shellcheck disable=SC2016 # literal shell-substitution syntax is the pattern.
    if printf '%s' "$cmd" | grep -Eq '`[^`]*(cargo|rustc|rustdoc)[^`]*`|\$\([^)]*(cargo|rustc|rustdoc)[^)]*\)'; then
        printf 'denied\tshell command substitution would execute Rust verification before the outer command\tcommand_substitution\tcommand substitution containing cargo/rustc/rustdoc must not be used for tracker or mail evidence\n'
        return
    fi

    # RCH payload-inspection commands may quote a Cargo command as data,
    # but they do not execute that command locally.
    if is_rch_payload_inspection_command "$cmd"; then
        printf 'allowed\trch payload-inspection command classifies Rust verifier payload without executing it\t-\t-\n'
        return
    fi

    # Tracker comments often need to quote the exact Cargo command that
    # RCH attempted. Treat that as evidence text, not an execution
    # request, after the command-substitution guard above has ruled out
    # `$(cargo ...)` and backtick forms.
    if is_tracker_evidence_command "$cmd"; then
        printf 'allowed\ttracker evidence command quotes Rust verifier text without executing it\t-\t-\n'
        return
    fi

    # Whitelist only remote-required `rch exec` cargo forms. Bare
    # `rch exec -- cargo ...` can fall back to local Cargo when topology
    # admission fails, so the tripwire denies it and points callers at
    # scripts/rch_verify.sh or RCH_REQUIRE_REMOTE=1.
    if is_rch_exec_command "$cmd"; then
        if printf '%s' "$cmd" | grep -Eq "(cargo|rustc|rustdoc)[[:space:]]"; then
            if printf '%s' "$cmd" | grep -Eq 'RCH_REQUIRE_REMOTE[[:space:]]*=[[:space:]]*1'; then
                printf 'allowed\tcargo wrapped through remote-required rch exec\t-\t-\n'
                return
            fi
            printf 'denied\trch exec Rust verifier command lacks RCH_REQUIRE_REMOTE=1\trch_exec_without_remote_required\tbare rch exec can fall back to local Cargo/Rust execution; use scripts/rch_verify.sh -- <cargo command> or prefix rch exec with RCH_REQUIRE_REMOTE=1\n'
            return
        fi
        printf 'allowed\trch exec command without Rust verifier payload\t-\t-\n'
        return
    fi

    # Whitelist the repo-local verifier wrapper. It performs the RCH-only
    # admission checks and is the expected agent-facing entrypoint.
    if printf '%s' "$cmd" | grep -Eq '(^|[[:space:]/.])scripts/rch_verify\.sh([[:space:]]|$)'; then
        printf 'allowed\tcargo wrapped through scripts/rch_verify.sh\t-\t-\n'
        return
    fi

    for tool in rustc rustdoc; do
        if printf '%s' "$cmd" | grep -Eq "(^|[[:space:]/])${tool}([[:space:]]|$)"; then
            printf 'denied\tdirect %s invocation bypasses the RCH wrapper\t%s\t%s invocation has no rch exec wrapper in the command string\n' \
                "$tool" "$tool" "$tool"
            return
        fi
    done

    # Detect the bare `cargo <forbidden-subcommand>` shape with no rch
    # prefix anywhere.
    subcommand=$(cargo_subcommand_from_command "$cmd")
    if is_forbidden_cargo_subcommand "$subcommand"; then
        detail="cargo $subcommand invocation has no rch exec wrapper in the command string"
    else
        subcommand=""
    fi

    if [ -z "$subcommand" ]; then
        printf 'allowed\tnot a forbidden cargo compilation subcommand\t-\t-\n'
        return
    fi

    # Bonus diagnostic: the bead body specifically cites the failure
    # where the caller set RCH_REQUIRE_REMOTE=1 but did NOT prefix with
    # `rch exec`. Surface that case with a more specific detail line.
    if printf '%s' "$cmd" | grep -Eq 'RCH_REQUIRE_REMOTE[[:space:]]*=[[:space:]]*1'; then
        detail="$detail (RCH_REQUIRE_REMOTE=1 was set but rch exec wrapper is absent — exact bd-1h8ji.2 failure mode)"
    fi

    printf 'denied\tdirect cargo %s invocation bypasses the RCH wrapper\t%s\t%s\n' \
        "$subcommand" "$subcommand" "$detail"
}

# True when a ps etime value is at least one hour (dd-hh:mm:ss or hh:mm:ss).
elapsed_at_least_one_hour() {
    local elapsed="$1"
    local days=0
    local hours
    case "$elapsed" in
        *-*)
            days=${elapsed%%-*}
            elapsed=${elapsed#*-}
            ;;
    esac
    case "$elapsed" in
        *:*:*) hours=${elapsed%%:*} ;;
        *) return 1 ;;
    esac
    case "$days:$hours" in
        *[!0-9:]*|:*) return 1 ;;
    esac
    [ "$days" -gt 0 ] || [ "$hours" -ge 1 ]
}

process_stat_for_pid() {
    local pid="$1"
    if [ -n "$PROCESS_STAT_TEXT_FIXTURE" ]; then
        printf '%s\n' "$PROCESS_STAT_TEXT_FIXTURE" |
            awk -v wanted_pid="$pid" '$1 == wanted_pid { print $2; exit }'
        return
    fi
    if [ -n "$PS_FIXTURE" ]; then
        return 0
    fi
    ps -o stat= -p "$pid" 2>/dev/null | tr -d ' '
}

process_descends_from_rust_analyzer_check() {
    local ps_text="$1"
    local child_pid="$2"
    local current_pid="$child_pid"
    local current_cmd
    local executable
    local subcommand
    local parent_pid
    local saw_cargo_check=false
    local depth=0

    while [ -n "$current_pid" ] && [ "$current_pid" != "0" ] && [ "$depth" -lt 32 ]; do
        current_cmd=$(parent_command_from_ps_output "$ps_text" "$current_pid")
        executable=$(printf '%s\n' "$current_cmd" | awk '{print $1}')
        case "$executable" in
            rust-analyzer|*/rust-analyzer)
                [ "$saw_cargo_check" = true ]
                return
                ;;
        esac
        if command_mentions_rust_tool "$current_cmd"; then
            subcommand=$(cargo_subcommand_from_command "$current_cmd")
            if [ -n "$subcommand" ]; then
                [ "$subcommand" = "check" ] || return 1
                saw_cargo_check=true
            fi
        fi
        parent_pid=$(parent_pid_from_ps_output "$ps_text" "$current_pid")
        if [ -z "$parent_pid" ] || [ "$parent_pid" = "0" ] || [ "$parent_pid" = "$current_pid" ]; then
            return 1
        fi
        current_pid="$parent_pid"
        depth=$((depth + 1))
    done
    return 1
}

blocking_process_count() {
    awk -F'\t' '
        $10 != "editor_tooling_informational" &&
        $10 != "unkillable_stale_informational" { count += 1 }
        END { print count + 0 }
    '
}

probe_processes() {
    # Scan running cargo/rustc processes for ones that target this
    # repo's worktree paths without an `rch exec` ancestor visible in
    # their command lines. A shell, tracker, or watcher process whose argv
    # merely stores a Cargo command as inert data is not itself a local Rust
    # build; if that shell actually executes Cargo, the cargo/rustc child is
    # reported by its own process row. This executable-boundary rule avoids
    # starving RCH admission on long-lived proof observers while preserving
    # fail-closed command classification before a shell command is spawned.
    #
    # Output rows:
    # <pid>\t<ppid>\t<elapsed>\t<command-kind>\t<subcommand>\t<cwd>\t<manifest>\t<workspace>\t<package-cache-lock>\t<policy-status>\t<short-command>\t<flagged-reason>\t<tmux-pane-id>\t<tmux-pane-pid>\t<tmux-locator>\t<tmux-current-path>\t<tmux-title>
    local ps_output
    ps_output=$(process_scan_ps_output 2>/dev/null || true)
    if [ -z "$ps_output" ]; then
        return 0
    fi
    local rust_process_lines
    rust_process_lines=$(printf '%s\n' "$ps_output" | grep -E "(^|[[:space:]/'\"(;])cargo([[:space:]]|$)|(^|[[:space:]/'\"(;])rustc([[:space:]]|$)|(^|[[:space:]/'\"(;])rustdoc([[:space:]]|$)" || true)
    if [ -z "$rust_process_lines" ]; then
        return 0
    fi
    local lock_holder_pids
    lock_holder_pids=$(package_cache_lock_pids)
    local tmux_panes
    tmux_panes=$(tmux_panes_output)
    # `ps` on macOS prints PID with leading spaces; normalize.
    printf '%s\n' "$rust_process_lines" | while IFS= read -r line; do
        local pid
        local ppid
        local elapsed
        local executable_name
        local cmd
        pid=$(printf '%s' "$line" | awk '{print $1}')
        ppid=$(printf '%s' "$line" | awk '{print $2}')
        elapsed=$(printf '%s' "$line" | awk '{print $3}')
        executable_name=$(printf '%s' "$line" | awk '{print $4}')
        cmd=$(printf '%s' "$line" | sed -E 's/^[[:space:]]*[0-9]+[[:space:]]+[0-9]+[[:space:]]+[^[:space:]]+[[:space:]]+[^[:space:]]+[[:space:]]+//')
        [ -n "$pid" ] || continue
        [ -n "$executable_name" ] || continue
        [ -n "$cmd" ] || continue
        process_executable_is_rust_tool "$executable_name" || continue
        # Skip our own shell + the ps invocation above.
        case "$cmd" in
            *check-local-cargo-tripwire*|*ps[[:space:]]-eo*) continue ;;
        esac
        # Skip the approved repo-local verifier wrapper. It may contain a
        # cargo command string, but it is the policy-compliant front door.
        case "$cmd" in
            *scripts/rch_verify.sh*) continue ;;
        esac
        # Stable rch_verify re-execs itself through `bash -s -- ...`, so
        # the wrapper process no longer has scripts/rch_verify.sh in argv.
        # That shell is policy-compliant data plumbing, not a local Cargo
        # process; any spawned local cargo/rustc child is still reported by
        # its own process row.
        if is_stable_rch_verify_wrapper_command "$cmd"; then
            continue
        fi
        # Skip read-only RCH diagnostics. The command line can include
        # `cargo check ...` as the payload being classified, but no local
        # Cargo process is spawned by these payload-inspection commands.
        if is_rch_payload_inspection_command "$cmd"; then
            continue
        fi
        # Skip tracker evidence commands. They can include `cargo test`
        # in a Beads comment body while the live process is `br`/wrapper
        # plumbing rather than Cargo.
        if is_tracker_evidence_process_command "$cmd"; then
            continue
        fi
        # Skip explicit SSH remote-proof launchers. Their argv can contain a
        # Cargo payload string, but the local process is ssh; any real local
        # cargo/rustc child remains visible as its own process row.
        if is_ssh_remote_rust_payload_command "$cmd"; then
            continue
        fi
        local matches_repo=false
        for hint in $REPO_PATH_HINTS; do
            case "$cmd" in
                *"$hint"*) matches_repo=true; break ;;
            esac
        done
        if [ "$matches_repo" != true ]; then
            local parent_cmd
            parent_cmd=$(parent_command_from_ps_output "$ps_output" "$ppid")
            for hint in $REPO_PATH_HINTS; do
                case "$parent_cmd" in
                    *"$hint"*) matches_repo=true; break ;;
                esac
            done
        fi
        local cwd="-"
        if [ "$matches_repo" != true ]; then
            cwd=$(process_cwd "$pid")
            case "$cwd" in
                "$PWD"*) matches_repo=true ;;
            esac
        fi
        # Only flag processes operating on this repo.
        [ "$matches_repo" = true ] || continue
        # Skip if an RCH exec helper appears anywhere in the command
        # (this is the remote-execution local launcher process). The
        # installed helper can be versioned, such as rch-manifestfix-*.
        if is_rch_exec_command "$cmd"; then
            continue
        fi
        local command_kind
        local subcommand
        local manifest_path
        local workspace_path
        local package_cache_lock_state
        local policy_status
        local reason
        local tmux_attribution
        command_kind=$(command_kind_from_command "$executable_name")
        subcommand=$(cargo_subcommand_from_command "$cmd")
        manifest_path=$(manifest_path_from_command "$cmd" "$cwd")
        workspace_path=$(workspace_path_from_manifest "$manifest_path" "$cwd")
        package_cache_lock_state=$(package_cache_lock_state "$pid" "$lock_holder_pids")
        policy_status=$(active_process_policy_status "$command_kind" "$subcommand" "$package_cache_lock_state")
        reason=$(active_process_reason "$command_kind" "$subcommand" "$package_cache_lock_state")
        # Editor flycheck exemption (bd-088ci): a cargo check spawned by
        # rust-analyzer is the OPERATOR'S editor doing check-on-save, not an
        # agent bypassing RCH. Report it (visibility) but classify it
        # informational so it does not block proof-broker admission — on a
        # dev Mac with an open editor the old classification starved every
        # compliant RCH dispatch on the machine.
        if process_descends_from_rust_analyzer_check "$ps_output" "$pid"; then
            policy_status="editor_tooling_informational"
            reason="cargo ${subcommand:-check} descended from rust-analyzer (editor check-on-save); informational, not an RCH bypass"
        fi
        # Unkillable-stale exemption (bd-088ci): a process stuck in
        # uninterruptible I/O wait (stat U/D, e.g. on a stalled external
        # volume) for an hour+ cannot be executing new builds and cannot be
        # killed; blocking admission on it wedges the machine until reboot.
        # Fixture-backed process state keeps this branch deterministic in the
        # self-test and prevents fixture pids from probing unrelated live pids.
        if [ "$policy_status" != "editor_tooling_informational" ]; then
            local live_stat
            live_stat=$(process_stat_for_pid "$pid")
            case "$live_stat" in
                U*|D*)
                    if elapsed_at_least_one_hour "$elapsed"; then
                        policy_status="unkillable_stale_informational"
                        reason="process in uninterruptible I/O wait (stat ${live_stat}) for ${elapsed}; cannot execute new builds or be killed — excluded from admission blocking"
                    fi
                    ;;
            esac
        fi
        tmux_attribution=$(tmux_pane_for_process "$ps_output" "$tmux_panes" "$pid")
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$pid" "$ppid" "$elapsed" "$command_kind" "$subcommand" "$cwd" \
            "$manifest_path" "$workspace_path" "$package_cache_lock_state" \
            "$policy_status" "$(printf '%s' "$cmd" | cut -c1-200)" "$reason" \
            "$tmux_attribution"
    done | sort -n -k1,1
}

canonical_worktree_path() {
    local top
    top=$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || true)
    if [ -n "$top" ] && [ -d "$top" ]; then
        (cd "$top" 2>/dev/null && pwd -P) || printf '%s\n' "$top"
    else
        printf '%s\n' "$PWD"
    fi
}

worktree_list_output() {
    if [ -n "$WORKTREE_FIXTURE" ]; then
        cat "$WORKTREE_FIXTURE"
        return
    fi
    git -C "$PWD" worktree list --porcelain 2>/dev/null || true
}

git_common_dir_for_worktree() {
    local worktree_path="$1"
    local common_dir="-"
    if [ -z "$WORKTREE_FIXTURE" ] && [ -d "$worktree_path" ]; then
        common_dir=$(git -C "$worktree_path" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)
        if [ -z "$common_dir" ]; then
            common_dir=$(git -C "$worktree_path" rev-parse --git-common-dir 2>/dev/null || true)
        fi
    fi
    [ -n "$common_dir" ] || common_dir="-"
    printf '%s\n' "$common_dir"
}

emit_forbidden_worktree_record() {
    local canonical="$1"
    local worktree_path="$2"
    local head="$3"
    local branch="$4"
    local detached="$5"
    local git_common_dir
    if [ -z "$worktree_path" ] || [ "$worktree_path" = "$canonical" ]; then
        return 0
    fi
    git_common_dir=$(git_common_dir_for_worktree "$worktree_path")
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$worktree_path" "$head" "$branch" "$detached" "$git_common_dir" \
        "critical" \
        "forbidden git worktree for single-checkout repo" \
        "Stop verification and ask the human before cleanup; AGENTS.md still forbids git worktree remove without explicit approval."
}

forbidden_worktrees_from_text() {
    local worktree_text="$1"
    local canonical="$2"
    local worktree_path=""
    local head="-"
    local branch="-"
    local detached=false
    {
        printf '%s\n' "$worktree_text"
        printf '\n'
    } | while IFS= read -r line; do
        case "$line" in
            worktree\ *)
                emit_forbidden_worktree_record "$canonical" "$worktree_path" "$head" "$branch" "$detached"
                worktree_path=${line#worktree }
                head="-"
                branch="-"
                detached=false
                ;;
            HEAD\ *) head=${line#HEAD } ;;
            branch\ *) branch=${line#branch } ;;
            detached) detached=true ;;
            "")
                emit_forbidden_worktree_record "$canonical" "$worktree_path" "$head" "$branch" "$detached"
                worktree_path=""
                head="-"
                branch="-"
                detached=false
                ;;
        esac
    done
}

probe_forbidden_worktrees() {
    local canonical
    local worktree_text
    canonical=$(canonical_worktree_path)
    worktree_text=$(worktree_list_output)
    forbidden_worktrees_from_text "$worktree_text" "$canonical" | sort
}

process_scan_ps_output() {
    if [ -n "$PS_FIXTURE" ]; then
        cat "$PS_FIXTURE"
        return
    fi
    # `ucomm` is the kernel-backed executable name. Unlike `command`/argv[0],
    # it remains `cargo` when a caller spoofs argv[0] with `exec -a`.
    ps -eo pid=,ppid=,etime=,ucomm=,command=
}

bounded_lsof() {
    if ! command -v lsof >/dev/null 2>&1; then
        return 127
    fi
    # Poll-and-abandon instead of any wait-based bound: on macOS an lsof
    # blocked in uninterruptible disk-wait (a stalled external or network
    # volume — lsof scans every open file system-wide, so ANY stalled mount
    # wedges it) survives SIGKILL, and `timeout`/`timeout -k`/perl-alarm all
    # wait for the unkillable child — which serialized every rch_verify
    # preflight on this host behind hung lsof zombies (observed 2026-08-10,
    # stalled /Volumes mount). Abandoning the child leaks one D-state
    # process the kernel reaps when the volume recovers; that beats an
    # infinite preflight hang.
    local out
    out=$(mktemp "${TMPDIR:-/tmp}/bounded_lsof.XXXXXX") || return 1
    lsof "$@" >"$out" 2>/dev/null &
    local lsof_pid=$!
    local waited=0
    while kill -0 "$lsof_pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
        sleep 0.1
        waited=$((waited + 1))
    done
    if kill -0 "$lsof_pid" 2>/dev/null; then
        kill -9 "$lsof_pid" 2>/dev/null || true
        disown "$lsof_pid" 2>/dev/null || true
        rm -f "$out"
        return 124
    fi
    local status=0
    wait "$lsof_pid" || status=$?
    cat "$out"
    rm -f "$out"
    return "$status"
}

timeout_command() {
    local candidate
    for candidate in /opt/homebrew/bin/timeout gtimeout timeout; do
        if [ -x "$candidate" ]; then
            printf '%s\n' "$candidate"
            return
        fi
        if command -v "$candidate" >/dev/null 2>&1; then
            command -v "$candidate"
            return
        fi
    done
}

parent_command_from_ps_output() {
    local ps_text="$1"
    local parent_pid="$2"
    [ -n "$parent_pid" ] || return 0
    printf '%s\n' "$ps_text" | awk -v wanted_pid="$parent_pid" '
        $1 == wanted_pid {
            sub(/^[[:space:]]*[0-9]+[[:space:]]+[0-9]+[[:space:]]+[^[:space:]]+[[:space:]]+[^[:space:]]+[[:space:]]+/, "")
            print
            exit
        }
    '
}

parent_pid_from_ps_output() {
    local ps_text="$1"
    local child_pid="$2"
    [ -n "$child_pid" ] || return 0
    printf '%s\n' "$ps_text" | awk -v wanted_pid="$child_pid" '$1 == wanted_pid { print $2; exit }'
}

tmux_panes_output() {
    if [ -n "$TMUX_PANES_TEXT_FIXTURE" ]; then
        printf '%s\n' "$TMUX_PANES_TEXT_FIXTURE"
        return
    fi
    if [ -n "$TMUX_PANES_FIXTURE" ]; then
        cat "$TMUX_PANES_FIXTURE"
        return
    fi
    if ! command -v tmux >/dev/null 2>&1; then
        return 0
    fi
    tmux list-panes -a -F '#{pane_id}	#{pane_pid}	#{session_name}:#{window_index}.#{pane_index}	#{pane_current_path}	#{pane_title}' 2>/dev/null || true
}

tmux_pane_record_for_pid() {
    local tmux_text="$1"
    local candidate_pid="$2"
    [ -n "$candidate_pid" ] || return 0
    printf '%s\n' "$tmux_text" | awk -F '\t' -v wanted_pid="$candidate_pid" '
        $2 == wanted_pid {
            pane_id = ($1 == "" ? "-" : $1)
            pane_pid = ($2 == "" ? "-" : $2)
            locator = ($3 == "" ? "-" : $3)
            current_path = ($4 == "" ? "-" : $4)
            title = ($5 == "" ? "-" : $5)
            print pane_id "\t" pane_pid "\t" locator "\t" current_path "\t" title
            exit
        }
    '
}

empty_tmux_pane_record() {
    printf -- '-\t-\t-\t-\t-\n'
}

tmux_pane_for_process() {
    local ps_text="$1"
    local tmux_text="$2"
    local pid="$3"
    local current_pid="$pid"
    local depth=0
    local record
    [ -n "$tmux_text" ] || { empty_tmux_pane_record; return; }
    while [ -n "$current_pid" ] && [ "$current_pid" != "0" ] && [ "$depth" -lt 32 ]; do
        record=$(tmux_pane_record_for_pid "$tmux_text" "$current_pid")
        if [ -n "$record" ]; then
            printf '%s\n' "$record"
            return
        fi
        current_pid=$(parent_pid_from_ps_output "$ps_text" "$current_pid")
        depth=$((depth + 1))
    done
    empty_tmux_pane_record
}

process_cwd() {
    local pid="$1"
    local cwd="-"
    if [ -z "$PS_FIXTURE" ]; then
        cwd=$(bounded_lsof -a -p "$pid" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p' | head -1 || true)
    fi
    [ -n "$cwd" ] || cwd="-"
    printf '%s\n' "$cwd"
}

command_mentions_rust_tool() {
    printf '%s' "$1" | grep -Eq "(^|[[:space:]/'\"(;])cargo([[:space:]]|$)|(^|[[:space:]/'\"(;])rustc([[:space:]]|$)|(^|[[:space:]/'\"(;])rustdoc([[:space:]]|$)"
}

process_executable_is_rust_tool() {
    local executable="$1"
    executable=${executable##*/}
    case "$executable" in
        cargo|rustc|rustdoc) return 0 ;;
        *) return 1 ;;
    esac
}

is_stable_rch_verify_wrapper_command() {
    printf '%s' "$1" | grep -Eq '(^|[[:space:]/])bash[[:space:]]+-s[[:space:]]+--([[:space:]]|$)' &&
        command_mentions_rust_tool "$1"
}

is_rch_payload_inspection_command() {
    is_rch_diagnose_command "$1" || is_rch_workers_capabilities_command "$1"
}

is_rch_diagnose_command() {
    printf '%s' "$1" | grep -Eq '(^|[[:space:]/])rch([._-][^[:space:]/]+)?([[:space:]]+--json)?[[:space:]]+diagnose([[:space:]]|$)'
}

is_rch_workers_capabilities_command() {
    printf '%s' "$1" | grep -Eq '(^|[[:space:]/])rch([._-][^[:space:]/]+)?([[:space:]]+--json)?[[:space:]]+workers[[:space:]]+capabilities([[:space:]]|$)'
}

is_rch_exec_command() {
    printf '%s' "$1" | grep -Eq '(^|[[:space:]/])rch([._-][^[:space:]/]+)?([[:space:]]+--json)?[[:space:]]+exec([[:space:]]|--)'
}

is_ssh_remote_rust_payload_command() {
    printf '%s' "$1" | grep -Eq '(^|[[:space:]/])ssh([[:space:]]|$)' &&
        command_mentions_rust_tool "$1"
}

is_tracker_evidence_command() {
    local cmd="$1"
    # Do not bless shell command lists that run Cargo after the tracker
    # write. Plain prose such as "Command: cargo test ..." is handled by
    # the evidence-command shape below.
    if printf '%s' "$cmd" | grep -Eq '(^|[;&|])[[:space:]]*(cargo|rustc|rustdoc)([[:space:]]|$)'; then
        return 1
    fi
    printf '%s' "$cmd" | grep -Eq '(^|[[:space:]/.])scripts/br_retry\.sh[[:space:]]+comments[[:space:]]+add([[:space:]]|$)|(^|[[:space:]/])br[[:space:]]+comments[[:space:]]+add([[:space:]]|$)'
}

is_tracker_evidence_process_command() {
    local cmd="$1"
    # `ps` flattens argv and loses the quote boundary around `--message`.
    # A direct `br comments add ... --message "...; cargo --locked ..."`
    # process therefore looks like a shell command list to
    # is_tracker_evidence_command even though `br` receives the semicolon and
    # Cargo text as inert prose. Trust only the live executable boundary here:
    # a direct br process is evidence plumbing, while `sh -c 'br ...; cargo'`
    # still starts with sh and remains visible to the ordinary Cargo scan.
    if printf '%s' "$cmd" | grep -Eq '^([^[:space:]]*/)?br[[:space:]]+comments[[:space:]]+add([[:space:]]|$)'; then
        return 0
    fi
    is_tracker_evidence_command "$cmd"
}

command_kind_from_command() {
    local cmd="$1"
    if printf '%s' "$cmd" | grep -Eq "(^|[[:space:]/'\"(;])rustdoc([[:space:]]|$)"; then
        printf 'rustdoc\n'
    elif printf '%s' "$cmd" | grep -Eq "(^|[[:space:]/'\"(;])rustc([[:space:]]|$)"; then
        printf 'rustc\n'
    else
        printf 'cargo\n'
    fi
}

cargo_subcommand_from_command() {
    printf '%s\n' "$1" | awk \
        -v forbidden_list="$FORBIDDEN_CARGO_SUBCOMMANDS" \
        -v read_only_list="$READ_ONLY_CARGO_SUBCOMMANDS" '
        BEGIN {
            split(forbidden_list, forbidden_words, " ")
            for (forbidden_index in forbidden_words) {
                forbidden[forbidden_words[forbidden_index]] = 1
            }
            split(read_only_list, read_only_words, " ")
            for (read_only_index in read_only_words) {
                read_only[read_only_words[read_only_index]] = 1
            }
        }
        function clean(value) {
            gsub(/^[^A-Za-z0-9_+\/.-]+/, "", value)
            gsub(/[^A-Za-z0-9_+\/.-]+$/, "", value)
            return value
        }
        function is_cargo_token(value) {
            return value == "cargo" || value ~ /\/cargo$/
        }
        function option_takes_value(value) {
            return value == "--config" || value == "--color" || value == "-Z" || value == "-C"
        }
        function is_standalone_global_option(value) {
            return value == "--locked" || value == "--frozen" || value == "--offline" ||
                value == "--verbose" || value == "-v" || value == "-vv" || value == "-vvv" ||
                value == "--quiet" || value == "-q"
        }
        function is_attached_global_option(value) {
            return value ~ /^--config=/ || value ~ /^--color=/ || value ~ /^-Z./ || value ~ /^-C./
        }
        {
            after_cargo = 0
            skip_next = 0
            first_candidate = ""
            for (i = 1; i <= NF; i++) {
                word = clean($i)
                if (!after_cargo) {
                    if (is_cargo_token(word)) {
                        after_cargo = 1
                    }
                    continue
                }
                if (skip_next) {
                    skip_next = 0
                    continue
                }
                if (word == "" || word ~ /^[A-Za-z_][A-Za-z0-9_]*=.*/) {
                    continue
                }
                if (word in forbidden) {
                    print word
                    exit
                }
                if (word ~ /^\+[^[:space:]]+$/) {
                    continue
                }
                if (option_takes_value(word)) {
                    skip_next = 1
                    continue
                }
                if (is_standalone_global_option(word) || is_attached_global_option(word)) {
                    continue
                }
                if (word == "--" || word ~ /^-/) {
                    continue
                }
                if (first_candidate == "") {
                    first_candidate = word
                    if (word in read_only) {
                        print word
                        exit
                    }
                }
            }
            if (first_candidate != "") {
                print first_candidate
            }
        }
    '
}

is_forbidden_cargo_subcommand() {
    local candidate="$1"
    [ -n "$candidate" ] || return 1
    for forbidden in $FORBIDDEN_CARGO_SUBCOMMANDS; do
        if [ "$candidate" = "$forbidden" ]; then
            return 0
        fi
    done
    return 1
}

manifest_path_from_command() {
    local cmd="$1"
    local cwd="$2"
    local manifest
    manifest=$(printf '%s\n' "$cmd" | awk '
        {
            for (i = 1; i <= NF; i++) {
                if ($i == "--manifest-path" && i + 1 <= NF) {
                    value = $(i + 1)
                    gsub(/^[^A-Za-z0-9_\/.-]+/, "", value)
                    gsub(/[^A-Za-z0-9_\/.-]+$/, "", value)
                    print value
                    exit
                }
                if ($i ~ /^--manifest-path=/) {
                    sub(/^--manifest-path=/, "", $i)
                    gsub(/^[^A-Za-z0-9_\/.-]+/, "", $i)
                    gsub(/[^A-Za-z0-9_\/.-]+$/, "", $i)
                    print $i
                    exit
                }
            }
        }
    ')
    if [ -z "$manifest" ] && [ "$cwd" != "-" ]; then
        case "$cwd" in
            "$PWD"*) manifest="$PWD/Cargo.toml" ;;
        esac
    fi
    [ -n "$manifest" ] || manifest="-"
    printf '%s\n' "$manifest"
}

workspace_path_from_manifest() {
    local manifest_path="$1"
    local cwd="$2"
    case "$manifest_path" in
        */Cargo.toml)
            dirname "$manifest_path"
            return
            ;;
    esac
    if [ "$cwd" != "-" ]; then
        printf '%s\n' "$cwd"
    else
        printf '%s\n' "$PWD"
    fi
}

package_cache_lock_state() {
    local pid="$1"
    local lock_holder_pids="${2:-}"
    if printf '%s\n' "$lock_holder_pids" | grep -Fxq "__UNAVAILABLE__"; then
        printf 'unavailable\n'
        return
    fi
    if [ -n "$PACKAGE_CACHE_PIDS_FIXTURE" ]; then
        if printf '%s\n' "$lock_holder_pids" | grep -Fxq "$pid"; then
            printf 'held\n'
        else
            printf 'not_observed\n'
        fi
        return
    fi
    if [ -n "$PS_FIXTURE" ]; then
        printf 'unavailable\n'
        return
    fi
    if printf '%s\n' "$lock_holder_pids" | grep -Fxq "$pid"; then
        printf 'held\n'
    else
        printf 'not_observed\n'
    fi
}

package_cache_lock_pids() {
    if [ -n "$PACKAGE_CACHE_PIDS_FIXTURE" ]; then
        printf '%s\n' "$PACKAGE_CACHE_PIDS_FIXTURE" | tr ', ' '\n' | grep -E '^[0-9]+$' || true
        return
    fi
    if [ -n "$PS_FIXTURE" ]; then
        return
    fi
    local cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    local lock_path="$cargo_home/.package-cache"
    if [ ! -e "$lock_path" ]; then
        return
    fi
    local holders
    if holders=$(bounded_lsof -t "$lock_path" 2>/dev/null); then
        printf '%s\n' "$holders" | sort -u
    else
        printf '__UNAVAILABLE__\n'
    fi
}

is_read_only_cargo_subcommand() {
    local subcommand="$1"
    for allowed in $READ_ONLY_CARGO_SUBCOMMANDS; do
        if [ "$subcommand" = "$allowed" ]; then
            return 0
        fi
    done
    return 1
}

active_process_policy_status() {
    local command_kind="$1"
    local subcommand="$2"
    local package_cache_lock_state="$3"
    if [ "$command_kind" != "cargo" ]; then
        printf 'local_rust_tool_disallowed\n'
    elif is_read_only_cargo_subcommand "$subcommand"; then
        if [ "$package_cache_lock_state" = "held" ]; then
            printf 'local_cargo_read_only_lock_holder\n'
        else
            printf 'local_cargo_read_only_observed\n'
        fi
    else
        printf 'local_cargo_disallowed\n'
    fi
}

active_process_reason() {
    local command_kind="$1"
    local subcommand="$2"
    local package_cache_lock_state="$3"
    if [ "$command_kind" != "cargo" ]; then
        printf 'local %s process targeting this repo without rch exec\n' "$command_kind"
    elif is_read_only_cargo_subcommand "$subcommand"; then
        if [ "$package_cache_lock_state" = "held" ]; then
            printf 'read-only cargo %s process holds the Cargo package-cache lock and can block RCH verification\n' "$subcommand"
        else
            printf 'read-only cargo %s process targeting this repo; verify it is not being used as fallback proof\n' "$subcommand"
        fi
    else
        printf 'local cargo %s process targeting this repo without rch exec\n' "$subcommand"
    fi
}

emit_human_cmd() {
    local allowed="$1"
    local reason="$2"
    local subcommand="$3"
    local detail="$4"
    if [ "$allowed" = "allowed" ]; then
        printf '[rch tripwire] allowed: %s\n' "$reason"
        return 0
    fi
    printf '[rch tripwire] DENIED: %s\n' "$reason"
    if [ "$detail" != "-" ] && [ -n "$detail" ]; then
        printf '  detail: %s\n' "$detail"
    fi
    printf '  fix: prefix with %s\n' "$REQUIRED_REMOTE_WRAPPER"
}

emit_json_cmd() {
    local allowed="$1"
    local reason="$2"
    local subcommand="$3"
    local detail="$4"
    if command -v jq >/dev/null 2>&1; then
        jq -cn \
            --arg schema "$REPORT_SCHEMA" \
            --arg mode "cmd_classify" \
            --arg allowed "$allowed" \
            --arg reason "$reason" \
            --arg subcommand "$subcommand" \
            --arg detail "$detail" \
            --arg required_remote_wrapper "$REQUIRED_REMOTE_WRAPPER" \
            '{
                schema:$schema,
                mode:$mode,
                allowed:$allowed,
                reason:$reason,
                subcommand:$subcommand,
                detail:$detail,
                localBuildPolicy:{
                    policy:"rch_only",
                    status:(if $allowed == "allowed" then "satisfied" else "blocked" end),
                    commandScope:"planned_command",
                    allowedReadOnlyCargoSubcommands:["metadata","locate-project","pkgid","tree"]
                },
                requiredRemoteWrapper:$required_remote_wrapper,
                detectedLocalBuilds:(
                    if $allowed == "denied" then
                        [{
                            policyStatus:"local_cargo_disallowed",
                            commandKind:(if ($subcommand == "rustc" or $subcommand == "rustdoc") then $subcommand elif $subcommand == "rch_exec_without_remote_required" then "rust_verifier" else "cargo" end),
                            subcommand:$subcommand,
                            reason:$reason,
                            detail:$detail
                        }]
                    else [] end
                ),
                repairActions:(
                    if $allowed == "denied" then
                        if $subcommand == "command_substitution" then
                            [{
                                priority:1,
                                kind:"avoid_shell_command_substitution",
                                command:null,
                                message:"Do not embed verifier commands in shell command substitution; pass evidence as plain quoted prose, a direct tool call, or an existing artifact path."
                            }]
                        else
                            [{
                                priority:1,
                                kind:"use_remote_wrapper",
                                command:$required_remote_wrapper,
                                message:"Run Rust verification through the repo RCH wrapper; do not retry local Cargo."
                            }]
                        end
                    else [] end
                ),
                evidence:[{
                    kind:"planned_command_classification",
                    result:$allowed,
                    subcommand:$subcommand,
                    reason:$reason
                }]
            }'
    else
        printf '{"schema":"%s","mode":"cmd_classify","allowed":"%s","reason":"%s","subcommand":"%s","detail":"%s"}\n' \
            "$REPORT_SCHEMA" "$allowed" "$reason" "$subcommand" "$detail"
    fi
}

emit_human_probe() {
    local body="$1"
    local count="$2"
    local worktree_body="$3"
    local worktree_count="$4"
    if [ "$count" -eq 0 ] && [ "$worktree_count" -eq 0 ]; then
        printf '[rch tripwire] clean: no local cargo/rustc processes targeting this repo without rch exec and no forbidden git worktrees.\n'
        return 0
    fi
    if [ "$count" -gt 0 ]; then
        printf '[rch tripwire] %d local cargo/rustc process(es) running without rch exec wrapper:\n' "$count"
        printf '%s' "$body" | while IFS=$(printf '\t') read -r pid ppid elapsed command_kind subcommand cwd manifest_path workspace_path package_cache_lock_state policy_status short_cmd reason tmux_pane_id tmux_pane_pid tmux_locator tmux_current_path tmux_title; do
            [ -n "$pid" ] || continue
            printf '  - pid=%s ppid=%s elapsed=%s kind=%s subcommand=%s policy=%s cwd=%s manifest=%s package_cache_lock=%s tmux_pane=%s tmux_pane_pid=%s tmux_locator=%s tmux_path=%s tmux_title=%s reason=%s\n      command: %s\n' \
                "$pid" "$ppid" "$elapsed" "$command_kind" "$subcommand" "$policy_status" "$cwd" "$manifest_path" "$package_cache_lock_state" "$tmux_pane_id" "$tmux_pane_pid" "$tmux_locator" "$tmux_current_path" "$tmux_title" "$reason" "$short_cmd"
        done
        printf '  suggestion: investigate the offending shell; never automatically kill processes here.\n'
    fi
    if [ "$worktree_count" -gt 0 ]; then
        printf '[rch tripwire] %d forbidden git worktree(s) present for this single-checkout repo:\n' "$worktree_count"
        printf '%s' "$worktree_body" | while IFS=$(printf '\t') read -r path head branch detached git_common_dir severity reason operator_action; do
            [ -n "$path" ] || continue
            printf '  - path=%s head=%s branch=%s detached=%s severity=%s git_common_dir=%s reason=%s\n      operator_action: %s\n' \
                "$path" "$head" "$branch" "$detached" "$severity" "$git_common_dir" "$reason" "$operator_action"
        done
        printf '  suggestion: stop RCH proof until a human explicitly authorizes any cleanup/adoption action.\n'
    fi
}

path_available_bytes() {
    local path="$1"
    if [ -z "$path" ] || [ ! -e "$path" ]; then
        printf 'null'
        return
    fi
    local available_bytes
    available_bytes=$(df -Pk "$path" 2>/dev/null | awk 'NR==2 {printf "%.0f", $4 * 1024}')
    if [ -n "$available_bytes" ]; then
        printf '%s' "$available_bytes"
    else
        printf 'null'
    fi
}

disk_context_json() {
    local workspace_path="$PWD"
    local cargo_target="${CARGO_TARGET_DIR:-}"
    local tmpdir="${TMPDIR:-}"
    local workspace_free_bytes
    local cargo_target_free_bytes="null"
    local tmpdir_free_bytes="null"
    local external_drive_mounted=false

    workspace_free_bytes=$(path_available_bytes "$workspace_path")
    if [ -n "$cargo_target" ]; then
        cargo_target_free_bytes=$(path_available_bytes "$cargo_target")
    fi
    if [ -n "$tmpdir" ]; then
        tmpdir_free_bytes=$(path_available_bytes "$tmpdir")
    fi
    if [ -d /Volumes/USBNVME16TB ]; then
        external_drive_mounted=true
    fi

    jq -cn \
        --arg workspace_path "$workspace_path" \
        --arg cargo_target_dir "$cargo_target" \
        --arg tmpdir "$tmpdir" \
        --argjson workspace_free_bytes "$workspace_free_bytes" \
        --argjson cargo_target_free_bytes "$cargo_target_free_bytes" \
        --argjson tmpdir_free_bytes "$tmpdir_free_bytes" \
        --argjson external_drive_mounted "$external_drive_mounted" \
        '{
            workspace_path:$workspace_path,
            workspace_free_bytes:$workspace_free_bytes,
            cargo_target_dir:(if $cargo_target_dir | length > 0 then $cargo_target_dir else null end),
            cargo_target_free_bytes:$cargo_target_free_bytes,
            tmpdir:(if $tmpdir | length > 0 then $tmpdir else null end),
            tmpdir_free_bytes:$tmpdir_free_bytes,
            external_drive_mounted:$external_drive_mounted
        }'
}

emit_json_probe() {
    local body="$1"
    local count="$2"
    local worktree_body="$3"
    local worktree_count="$4"
    local processes_json="[]"
    local worktrees_json="[]"
    local disk_context="{}"
    local canonical_worktree
    canonical_worktree=$(canonical_worktree_path)
    if [ -n "$body" ] && command -v jq >/dev/null 2>&1; then
        # Let jq consume raw tab-separated lines so process commands with
        # backslashes, quotes, or shell escapes are JSON-escaped correctly.
        processes_json=$(printf '%s' "$body" |
            jq -R -s '
                split("\n")
                | map(select(length > 0) | split("\t") | select(length >= 12) | {
                    pid:.[0],
                    ppid:.[1],
                    elapsed:.[2],
                    command_kind:.[3],
                    subcommand:.[4],
                    cwd:.[5],
                    manifestPath:.[6],
                    workspacePath:.[7],
                    packageCacheLockState:.[8],
                    packageCacheLockHeld:(if .[8] == "held" then true elif .[8] == "not_observed" then false else null end),
                    policyStatus:.[9],
                    command:.[10],
                    reason:.[11],
                    tmuxPane:{
                        paneId:(if (.[12] // "-") == "-" then null else .[12] end),
                        panePid:(if (.[13] // "-") == "-" then null else .[13] end),
                        locator:(if (.[14] // "-") == "-" then null else .[14] end),
                        currentPath:(if (.[15] // "-") == "-" then null else .[15] end),
                        title:(if (.[16] // "-") == "-" then null else .[16] end)
                    }
                })
            ')
    fi
    if [ -n "$worktree_body" ] && command -v jq >/dev/null 2>&1; then
        worktrees_json=$(printf '%s' "$worktree_body" |
            jq -R -s '
                split("\n")
                | map(select(length > 0) | split("\t") | select(length >= 8) | {
                    path:.[0],
                    head:.[1],
                    branch:(if .[2] == "-" then null else .[2] end),
                    detached:(.[3] == "true"),
                    gitCommonDir:(if .[4] == "-" then null else .[4] end),
                    severity:.[5],
                    reason:.[6],
                    operatorAction:.[7]
                })
            ')
    fi
    if command -v jq >/dev/null 2>&1; then
        disk_context=$(disk_context_json)
    fi
    local status="ok"
    if [ "$count" -gt 0 ] || [ "$worktree_count" -gt 0 ]; then status="bypass_detected"; fi
    if command -v jq >/dev/null 2>&1; then
        jq -cn \
            --arg schema "$REPORT_SCHEMA" \
            --arg mode "probe_processes" \
            --arg status "$status" \
            --arg required_remote_wrapper "$REQUIRED_REMOTE_WRAPPER" \
            --arg canonical_worktree "$canonical_worktree" \
            --argjson count "$count" \
            --argjson worktree_count "$worktree_count" \
            --argjson processes "$processes_json" \
            --argjson worktrees "$worktrees_json" \
            --argjson disk_context "$disk_context" \
            '($processes | map({
                policyStatus:.policyStatus,
                pid:.pid,
                ppid:.ppid,
                elapsed:.elapsed,
                commandKind:.command_kind,
                subcommand:.subcommand,
                cwd:.cwd,
                manifestPath:.manifestPath,
                workspacePath:.workspacePath,
                packageCacheLockState:.packageCacheLockState,
                packageCacheLockHeld:.packageCacheLockHeld,
                tmuxPane:.tmuxPane,
                command:.command,
                reason:.reason
            })) as $detected |
            {
                schema:$schema,
                mode:$mode,
                status:$status,
                count:$count,
                forbiddenWorktreeCount:$worktree_count,
                processes:$processes,
                forbiddenWorktrees:$worktrees,
                disk_pressure_context:$disk_context,
                localBuildPolicy:{
                    policy:"rch_only",
                    status:(if ($count > 0 or $worktree_count > 0) then "blocked" else "satisfied" end),
                    commandScope:"active_process_scan",
                    allowedReadOnlyCargoSubcommands:["metadata","locate-project","pkgid","tree"]
                },
                worktreePolicy:{
                    policy:"single_canonical_worktree",
                    status:(if $worktree_count > 0 then "blocked" else "satisfied" end),
                    commandScope:"git_worktree_list_porcelain",
                    canonicalWorktree:$canonical_worktree,
                    forbiddenWorktreeCount:$worktree_count
                },
                requiredRemoteWrapper:$required_remote_wrapper,
                detectedLocalBuilds:$detected,
                repairActions:(
                    (if $count > 0 then
                        [{
                            priority:1,
                            kind:"inspect_shell_without_killing",
                            command:null,
                            message:"Inspect the reported process owner and command; this detector never kills or cleans up processes."
                        }]
                    else [] end) +
                    (if $worktree_count > 0 then
                        [{
                            priority:2,
                            kind:"request_human_worktree_cleanup_approval",
                            command:null,
                            message:"A forbidden git worktree exists. Do not run git worktree remove, delete files, checkout, reset, stash, or mutate it without explicit human approval."
                        }]
                    else [] end)
                ),
                evidence:[
                    {
                        kind:"active_process_scan",
                        result:(if $count > 0 then "bypass_detected" else "ok" end),
                        processCount:$count,
                        diskPressureContext:$disk_context
                    },
                    {
                        kind:"forbidden_worktree_scan",
                        result:(if $worktree_count > 0 then "bypass_detected" else "ok" end),
                        canonicalWorktree:$canonical_worktree,
                        forbiddenWorktreeCount:$worktree_count
                    }
                ]
            }'
    else
        printf '{"schema":"%s","mode":"probe_processes","status":"%s","count":%d,"forbiddenWorktreeCount":%d,"processes":[],"forbiddenWorktrees":[]}\n' \
            "$REPORT_SCHEMA" "$status" "$count" "$worktree_count"
    fi
}

run_self_test() {
    # Direct cargo test → DENIED.
    local result
    result=$(classify_command "cargo test --lib happy_path")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: direct cargo test must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Direct cargo build with env prefix → DENIED.
    result=$(classify_command "RCH_REQUIRE_REMOTE=1 cargo build --release")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: env-prefixed cargo build must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Direct cargo doc → DENIED.
    result=$(classify_command "cargo doc --no-deps")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: cargo doc must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Cargo inside shell command substitution → DENIED before the
    # tracker/mail command can receive its argument.
    # shellcheck disable=SC2016 # self-test must pass literal backticks.
    result=$(classify_command 'br comments add bd-1 --message "`cargo test --lib foo`"')
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: backtick cargo substitution must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Rustdoc inside dollar-paren substitution → DENIED.
    # shellcheck disable=SC2016 # self-test must pass literal dollar-paren substitution.
    result=$(classify_command 'am send --body "$(rustdoc --test src/lib.rs)"')
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: dollar-paren rustdoc substitution must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Even an RCH-wrapped Cargo command inside substitution is denied:
    # command substitution is not an evidence transport.
    # shellcheck disable=SC2016 # self-test must pass literal dollar-paren substitution.
    result=$(classify_command 'br comments add bd-1 --message "$(scripts/rch_verify.sh -- cargo test --lib foo)"')
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: rch wrapper inside command substitution must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Tracker evidence may quote the exact Cargo command that RCH
    # attempted without executing it locally.
    result=$(classify_command 'br comments add bd-1 --message "RCH proof command: cargo test --lib foo"')
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: plain tracker evidence mentioning cargo must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    result=$(classify_command 'bash scripts/br_retry.sh comments add bd-1 --message "RCH proof command: cargo test --lib foo"')
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: br_retry tracker evidence mentioning cargo must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    result=$(classify_command 'br comments add bd-1 --message ok; cargo test --lib foo')
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: tracker command followed by cargo execution must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # A shell command that directly executes Cargo is still denied before
    # spawn even though the retrospective process scan reports only actual
    # cargo/rustc/rustdoc executables.
    result=$(classify_command "/bin/zsh -c 'cargo test --lib foo'")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: shell cargo execution must be denied before spawn; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Direct rustc → DENIED.
    result=$(classify_command "rustc src/main.rs")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: direct rustc must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Direct rustdoc → DENIED.
    result=$(classify_command "rustdoc --test src/lib.rs")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: direct rustdoc must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Absolute cargo binary path → DENIED.
    result=$(classify_command "/Users/jemanuel/.cargo/bin/cargo test --lib")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: absolute cargo path must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Cargo toolchain and global flags still execute a compile subcommand locally.
    result=$(classify_command "cargo +nightly test --lib")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: cargo +toolchain test must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    result=$(classify_command "cargo --locked test --lib")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: cargo --locked test must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    result=$(classify_command "cargo -Z timings test --lib")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: cargo -Z timings test must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    result=$(classify_command "cargo --config 'build.rustflags = [\"-Dwarnings\"]' test --lib")
    case "$result" in
        denied*) ;;
        *) printf 'self-test FAILED: cargo --config with spaced value before test must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Bare rch exec can fall back to local Cargo → DENIED.
    result=$(classify_command "rch exec -- env TMPDIR=/tmp cargo test --lib foo")
    case "$result" in
        denied*'rch_exec_without_remote_required'*) ;;
        *) printf 'self-test FAILED: bare rch exec wrapper must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Bare versioned RCH helper can still fall back locally → DENIED.
    result=$(classify_command "/Users/jemanuel/.local/bin/rch-manifestfix-20260605-5 exec -- env TMPDIR=/tmp cargo test --lib foo")
    case "$result" in
        denied*'rch_exec_without_remote_required'*) ;;
        *) printf 'self-test FAILED: bare versioned rch helper must be denied; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Remote-required rch exec → ALLOWED.
    result=$(classify_command "RCH_REQUIRE_REMOTE=1 rch exec -- env TMPDIR=/tmp cargo test --lib foo")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: remote-required rch exec wrapper must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Remote-required versioned RCH helper → ALLOWED.
    result=$(classify_command "RCH_REQUIRE_REMOTE=1 /Users/jemanuel/.local/bin/rch-manifestfix-20260605-5 exec -- env TMPDIR=/tmp cargo test --lib foo")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: remote-required versioned rch helper must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Wrapped through the repo verifier → ALLOWED.
    result=$(classify_command "scripts/rch_verify.sh -- cargo test --lib foo")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: scripts/rch_verify.sh wrapper must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Env-prefixed repo verifier → ALLOWED.
    result=$(classify_command "RCH_REQUIRE_REMOTE=1 ./scripts/rch_verify.sh -- cargo test --lib foo")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: env-prefixed scripts/rch_verify.sh wrapper must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # cargo metadata is not a compile subcommand → ALLOWED.
    result=$(classify_command "cargo metadata --format-version 1")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: cargo metadata must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    result=$(classify_command "cargo metadata --filter-platform test")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: cargo metadata arguments that mention test must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Absolute path wrapped remote-required rch exec → ALLOWED.
    result=$(classify_command "RCH_REQUIRE_REMOTE=1 /Users/jemanuel/.local/bin/rch-manifestfix-20260605-5 exec -- env TMPDIR=/tmp cargo bench --bench foo")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: absolute-path remote-required rch exec must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # rch diagnose quotes a Cargo command as read-only data → ALLOWED.
    result=$(classify_command "/Volumes/USBNVME16TB/temp_agent_space/rch-candidate/extracted/rch diagnose --dry-run 'cargo check --lib --quiet' --json")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: rch diagnose dry-run payload must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Versioned rch diagnose quotes a Cargo command as read-only data → ALLOWED.
    result=$(classify_command "/Users/jemanuel/.local/bin/rch.20260519T213833Z.pre-1.0.24 diagnose --json cargo test --lib global -- --nocapture")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: versioned rch diagnose payload must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Worker capability refresh quotes a Cargo command as read-only data → ALLOWED.
    result=$(classify_command "rch workers capabilities --refresh --command cargo test --lib lod_ -- --nocapture --json")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: rch workers capabilities payload must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    # Empty command → ALLOWED.
    result=$(classify_command "")
    case "$result" in
        allowed*) ;;
        *) printf 'self-test FAILED: empty command must be allowed; got %s\n' "$result" >&2; exit 1 ;;
    esac
    local old_worktree_fixture="$WORKTREE_FIXTURE"
    local clean_worktree_porcelain
    local forbidden_worktree_porcelain
    local multiple_worktree_porcelain
    local worktree_body
    local worktree_count
    WORKTREE_FIXTURE="self-test"
    clean_worktree_porcelain=$(cat <<'EOF'
worktree /Users/jemanuel/projects/eidetic_engine_cli
HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
branch refs/heads/main
EOF
)
    worktree_body=$(forbidden_worktrees_from_text "$clean_worktree_porcelain" "/Users/jemanuel/projects/eidetic_engine_cli")
    if [ -n "$worktree_body" ]; then
        printf 'self-test FAILED: canonical-only worktree fixture must be clean; got %s\n' "$worktree_body" >&2
        exit 1
    fi
    forbidden_worktree_porcelain=$(cat <<'EOF'
worktree /Users/jemanuel/projects/eidetic_engine_cli
HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
branch refs/heads/main

worktree /Users/jemanuel/projects/ee-clean-verify
HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
detached
EOF
)
    worktree_body=$(forbidden_worktrees_from_text "$forbidden_worktree_porcelain" "/Users/jemanuel/projects/eidetic_engine_cli")
    worktree_count=$(printf '%s' "$worktree_body" | grep -c . || true)
    if [ "$worktree_count" -ne 1 ] || ! printf '%s' "$worktree_body" | grep -Fq '/Users/jemanuel/projects/ee-clean-verify'; then
        printf 'self-test FAILED: detached forbidden worktree fixture must produce one row; got %s\n' "$worktree_body" >&2
        exit 1
    fi
    multiple_worktree_porcelain=$(cat <<'EOF'
worktree /Users/jemanuel/projects/eidetic_engine_cli
HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
branch refs/heads/main

worktree /Users/jemanuel/projects/ee-clean-verify
HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
detached

worktree /tmp/eidetic-engine-extra
HEAD cccccccccccccccccccccccccccccccccccccccc
branch refs/heads/feature
EOF
)
    worktree_body=$(forbidden_worktrees_from_text "$multiple_worktree_porcelain" "/Users/jemanuel/projects/eidetic_engine_cli")
    worktree_count=$(printf '%s' "$worktree_body" | grep -c . || true)
    WORKTREE_FIXTURE="$old_worktree_fixture"
    if [ "$worktree_count" -ne 2 ] || ! printf '%s' "$worktree_body" | grep -Fq 'refs/heads/feature'; then
        printf 'self-test FAILED: multiple-worktree fixture must produce two rows with branch data; got %s\n' "$worktree_body" >&2
        exit 1
    fi
    if command -v jq >/dev/null 2>&1; then
        repair_kind=$(emit_json_cmd \
            "denied" \
            "shell command substitution would execute Rust verification before the outer command" \
            "command_substitution" \
            "command substitution containing cargo/rustc/rustdoc must not be used for tracker or mail evidence" |
            jq -r '.repairActions[0].kind')
        case "$repair_kind" in
            avoid_shell_command_substitution) ;;
            *) printf 'self-test FAILED: command substitution repair action must be avoid_shell_command_substitution; got %s\n' "$repair_kind" >&2; exit 1 ;;
        esac
        local old_ps_fixture="$PS_FIXTURE"
        local old_package_cache_pids_fixture="$PACKAGE_CACHE_PIDS_FIXTURE"
        local old_tmux_panes_text_fixture="$TMUX_PANES_TEXT_FIXTURE"
        local old_process_stat_text_fixture="$PROCESS_STAT_TEXT_FIXTURE"
        PS_FIXTURE="tests/fixtures/rch_local_cargo_tripwire/process_scan_ps_fixture.txt"
        PACKAGE_CACHE_PIDS_FIXTURE="129"
        PROCESS_STAT_TEXT_FIXTURE=$(cat <<'EOF'
116 U+
124 U+
125 R+
127 D
128 U+
EOF
)
        TMUX_PANES_TEXT_FIXTURE=$(cat <<'EOF'
%22	102	eidetic_engine_cli:0.2	/Users/jemanuel/projects/eidetic_engine_cli	eidetic_engine_cli__cc_test
EOF
)
        fixture_body=$(probe_processes | sort -n -k1,1)
        fixture_count=$(printf '%s\n' "$fixture_body" | blocking_process_count)
        local unknown_informational_count
        unknown_informational_count=$(printf '999\t1\t00:01:00\tcargo\tcheck\t-\t-\t-\tnot_observed\tunknown_informational\tcargo check\tunknown status\n' | blocking_process_count)
        if [ "$unknown_informational_count" -ne 1 ]; then
            printf 'self-test FAILED: unknown informational-looking status must remain blocking\n' >&2
            exit 1
        fi
        fixture_report=$(emit_json_probe "$fixture_body" "$fixture_count" "$worktree_body" "$worktree_count")
        PS_FIXTURE="$old_ps_fixture"
        PACKAGE_CACHE_PIDS_FIXTURE="$old_package_cache_pids_fixture"
        TMUX_PANES_TEXT_FIXTURE="$old_tmux_panes_text_fixture"
        PROCESS_STAT_TEXT_FIXTURE="$old_process_stat_text_fixture"
        if ! printf '%s' "$fixture_report" | jq -e '
            .count == 11
            and .forbiddenWorktreeCount == 2
            and .localBuildPolicy.status == "blocked"
            and .worktreePolicy.status == "blocked"
            and ([.processes[].command] | map(contains("lsd")) | any | not)
            and ([.processes[].command] | map(contains("bash -s --")) | any | not)
            and ([.processes[].command] | map(contains("ssh -i")) | any | not)
            and any(.detectedLocalBuilds[]; .policyStatus == "local_cargo_read_only_lock_holder" and .subcommand == "metadata" and .packageCacheLockHeld == true and .tmuxPane.paneId == "%22" and .tmuxPane.locator == "eidetic_engine_cli:0.2" and .tmuxPane.title == "eidetic_engine_cli__cc_test")
            and any(.detectedLocalBuilds[]; .policyStatus == "local_cargo_disallowed" and .subcommand == "test" and .manifestPath == "/Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml" and .tmuxPane.paneId == null)
            and any(.detectedLocalBuilds[]; .policyStatus == "local_rust_tool_disallowed" and .commandKind == "rustc")
            and any(.detectedLocalBuilds[]; .policyStatus == "local_rust_tool_disallowed" and .commandKind == "rustdoc")
            and any(.detectedLocalBuilds[]; .policyStatus == "editor_tooling_informational" and .subcommand == "check")
            and any(.detectedLocalBuilds[]; .policyStatus == "editor_tooling_informational" and .commandKind == "rustc" and .pid == "118")
            and any(.detectedLocalBuilds[]; .policyStatus == "unkillable_stale_informational" and .pid == "116")
            and any(.detectedLocalBuilds[]; .policyStatus == "unkillable_stale_informational" and .pid == "127")
            and any(.detectedLocalBuilds[]; .policyStatus == "unkillable_stale_informational" and .pid == "128" and .elapsed == "01:00:00")
            and all(.detectedLocalBuilds[] | select(.pid == "120" or .pid == "121" or .pid == "122" or .pid == "123"); .policyStatus == "local_cargo_disallowed")
            and all(.detectedLocalBuilds[] | select(.pid == "124" or .pid == "125" or .pid == "126"); .policyStatus == "local_cargo_disallowed")
            and any(.forbiddenWorktrees[]; .path == "/Users/jemanuel/projects/ee-clean-verify" and .detached == true and .severity == "critical")
            and any(.forbiddenWorktrees[]; .path == "/tmp/eidetic-engine-extra" and .branch == "refs/heads/feature" and .detached == false)
            and any(.repairActions[]; .kind == "request_human_worktree_cleanup_approval")
            and any(.evidence[]; .kind == "forbidden_worktree_scan" and .result == "bypass_detected")
        ' >/dev/null; then
            printf 'self-test FAILED: process/worktree scan fixture did not produce expected classifications; got %s\n' "$fixture_report" >&2
            exit 1
        fi
    fi
    printf 'self-test PASSED: 30 classifier cases, JSON repair action, stable-wrapper/ssh exclusion, executable-boundary process/tmux fixture, and worktree fixtures produced expected outcomes\n'
    exit 0
}

if [ "$SELF_TEST" = true ]; then
    run_self_test
fi

case "$MODE" in
    cmd_classify)
        if [ "$CMD_PROVIDED" != true ]; then
            printf -- '--cmd requires a value\n' >&2
            usage >&2
            exit 2
        fi
        # An explicit `--cmd ""` is treated as a classifier query for the
        # empty command and returns allowed (the classifier already handles
        # empty input). Missing --cmd is a usage error because otherwise a
        # miswired hook would silently report "allowed: empty command".
        RESULT=$(classify_command "$CMD")
        ALLOWED=$(printf '%s' "$RESULT" | awk -F'\t' '{print $1}')
        REASON=$(printf '%s' "$RESULT" | awk -F'\t' '{print $2}')
        SUBCOMMAND=$(printf '%s' "$RESULT" | awk -F'\t' '{print $3}')
        DETAIL=$(printf '%s' "$RESULT" | awk -F'\t' '{print $4}')
        if [ "$JSON_OUTPUT" = true ]; then
            emit_json_cmd "$ALLOWED" "$REASON" "$SUBCOMMAND" "$DETAIL"
        else
            emit_human_cmd "$ALLOWED" "$REASON" "$SUBCOMMAND" "$DETAIL"
        fi
        if [ "$ALLOWED" = "denied" ]; then exit 1; fi
        exit 0
        ;;
    probe_processes)
        BODY=$(probe_processes | sort -n -k1,1 || true)
        if [ -n "$BODY" ]; then
            # Blocking count excludes editor-tooling rows (bd-088ci): they
            # are reported for visibility but must not gate RCH admission.
            COUNT=$(printf '%s\n' "$BODY" | blocking_process_count)
        else
            COUNT=0
        fi
        WORKTREE_BODY=$(probe_forbidden_worktrees || true)
        if [ -n "$WORKTREE_BODY" ]; then
            WORKTREE_COUNT=$(printf '%s' "$WORKTREE_BODY" | grep -c . || true)
        else
            WORKTREE_COUNT=0
        fi
        if [ "$JSON_OUTPUT" = true ]; then
            emit_json_probe "$BODY" "$COUNT" "$WORKTREE_BODY" "$WORKTREE_COUNT"
        else
            emit_human_probe "$BODY" "$COUNT" "$WORKTREE_BODY" "$WORKTREE_COUNT"
        fi
        if [ "$COUNT" -gt 0 ] || [ "$WORKTREE_COUNT" -gt 0 ]; then exit 1; fi
        exit 0
        ;;
esac
