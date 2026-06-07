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
#   scripts/check-local-cargo-tripwire.sh --probe-processes [--ps-file <fixture>] [--package-cache-pids <csv>] [--json]
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
PS_FIXTURE=""
PACKAGE_CACHE_PIDS_FIXTURE=""

usage() {
    sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'
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
        --cmd)
            shift
            if [ $# -eq 0 ]; then
                printf -- '--cmd requires a value\n' >&2
                usage >&2
                exit 2
            fi
            CMD="$1"
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
    for sub in $FORBIDDEN_CARGO_SUBCOMMANDS; do
        # Match "cargo <sub>" at start of line, after whitespace, or
        # after env-prefix tokens like `FOO=bar`, but NOT inside a
        # path-component such as "/usr/local/bin/cargo-test".
        if printf '%s' "$cmd" | grep -Eq "(^|[[:space:]/]|^[A-Z_]+=[^[:space:]]+([[:space:]]+[A-Z_]+=[^[:space:]]+)*[[:space:]]+)cargo[[:space:]]+${sub}([[:space:]]|$)"; then
            subcommand="$sub"
            detail="cargo $sub invocation has no rch exec wrapper in the command string"
            break
        fi
    done

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

probe_processes() {
    # Scan running cargo/rustc processes for ones that target this
    # repo's worktree paths without an `rch exec` ancestor visible in
    # their command lines. We rely on ps -eo command rather than the
    # process tree because ps -eo ppid is racy on macOS during fork.
    #
    # Output rows:
    # <pid>\t<ppid>\t<elapsed>\t<command-kind>\t<subcommand>\t<cwd>\t<manifest>\t<workspace>\t<package-cache-lock>\t<policy-status>\t<short-command>\t<flagged-reason>
    local ps_output
    ps_output=$(process_scan_ps_output 2>/dev/null || true)
    if [ -z "$ps_output" ]; then
        return 0
    fi
    # `ps` on macOS prints PID with leading spaces; normalize.
    printf '%s\n' "$ps_output" | while IFS= read -r line; do
        local pid
        local ppid
        local elapsed
        local cmd
        pid=$(printf '%s' "$line" | awk '{print $1}')
        ppid=$(printf '%s' "$line" | awk '{print $2}')
        elapsed=$(printf '%s' "$line" | awk '{print $3}')
        cmd=$(printf '%s' "$line" | sed -E 's/^[[:space:]]*[0-9]+[[:space:]]+[0-9]+[[:space:]]+[^[:space:]]+[[:space:]]+//')
        [ -n "$pid" ] || continue
        [ -n "$cmd" ] || continue
        command_mentions_rust_tool "$cmd" || continue
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
        # Skip explicit SSH remote-proof launchers. Their argv can contain a
        # Cargo payload string, but the local process is ssh; any real local
        # cargo/rustc child remains visible as its own process row.
        if is_ssh_remote_rust_payload_command "$cmd"; then
            continue
        fi
        local cwd="-"
        cwd=$(process_cwd "$pid")
        # Only flag processes operating on this repo.
        local matches_repo=false
        for hint in $REPO_PATH_HINTS; do
            case "$cmd" in
                *"$hint"*) matches_repo=true; break ;;
            esac
        done
        case "$cwd" in
            "$PWD"*) matches_repo=true ;;
        esac
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
        command_kind=$(command_kind_from_command "$cmd")
        subcommand=$(cargo_subcommand_from_command "$cmd")
        manifest_path=$(manifest_path_from_command "$cmd" "$cwd")
        workspace_path=$(workspace_path_from_manifest "$manifest_path" "$cwd")
        package_cache_lock_state=$(package_cache_lock_state "$pid")
        policy_status=$(active_process_policy_status "$command_kind" "$subcommand" "$package_cache_lock_state")
        reason=$(active_process_reason "$command_kind" "$subcommand" "$package_cache_lock_state")
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$pid" "$ppid" "$elapsed" "$command_kind" "$subcommand" "$cwd" \
            "$manifest_path" "$workspace_path" "$package_cache_lock_state" \
            "$policy_status" "$(printf '%s' "$cmd" | cut -c1-200)" "$reason"
    done | sort -n -k1,1
}

process_scan_ps_output() {
    if [ -n "$PS_FIXTURE" ]; then
        cat "$PS_FIXTURE"
        return
    fi
    ps -eo pid=,ppid=,etime=,command=
}

bounded_lsof() {
    if ! command -v lsof >/dev/null 2>&1; then
        return 127
    fi
    if command -v perl >/dev/null 2>&1; then
        perl -e 'alarm shift; exec @ARGV' 2 lsof "$@"
    else
        lsof "$@"
    fi
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
    printf '%s\n' "$1" | awk '
        {
            for (i = 1; i <= NF; i++) {
                word = $i
                gsub(/^[^A-Za-z0-9_\/.-]+/, "", word)
                gsub(/[^A-Za-z0-9_\/.-]+$/, "", word)
                if (word == "cargo" || word ~ /\/cargo$/) {
                    if (i + 1 <= NF) {
                        next_word = $(i + 1)
                        gsub(/^[^A-Za-z0-9_-]+/, "", next_word)
                        gsub(/[^A-Za-z0-9_-]+$/, "", next_word)
                        print next_word
                        exit
                    }
                }
            }
        }
    '
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
    if [ -n "$PACKAGE_CACHE_PIDS_FIXTURE" ]; then
        if printf '%s\n' "$PACKAGE_CACHE_PIDS_FIXTURE" | tr ', ' '\n' | grep -Fxq "$pid"; then
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
    local cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    local lock_path="$cargo_home/.package-cache"
    if [ ! -e "$lock_path" ]; then
        printf 'unavailable\n'
        return
    fi
    if bounded_lsof -a -p "$pid" "$lock_path" >/dev/null 2>&1; then
        printf 'held\n'
    else
        printf 'not_observed\n'
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
    if [ "$count" -eq 0 ]; then
        printf '[rch tripwire] clean: no local cargo/rustc processes targeting this repo without rch exec.\n'
        return 0
    fi
    printf '[rch tripwire] %d local cargo/rustc process(es) running without rch exec wrapper:\n' "$count"
    printf '%s' "$body" | while IFS=$(printf '\t') read -r pid ppid elapsed command_kind subcommand cwd manifest_path workspace_path package_cache_lock_state policy_status short_cmd reason; do
        [ -n "$pid" ] || continue
        printf '  - pid=%s ppid=%s elapsed=%s kind=%s subcommand=%s policy=%s cwd=%s manifest=%s package_cache_lock=%s reason=%s\n      command: %s\n' \
            "$pid" "$ppid" "$elapsed" "$command_kind" "$subcommand" "$policy_status" "$cwd" "$manifest_path" "$package_cache_lock_state" "$reason" "$short_cmd"
    done
    printf '  suggestion: investigate the offending shell; never automatically kill processes here.\n'
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
    local processes_json="[]"
    local disk_context="{}"
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
                    reason:.[11]
                })
            ')
    fi
    if command -v jq >/dev/null 2>&1; then
        disk_context=$(disk_context_json)
    fi
    local status="ok"
    if [ "$count" -gt 0 ]; then status="bypass_detected"; fi
    if command -v jq >/dev/null 2>&1; then
        jq -cn \
            --arg schema "$REPORT_SCHEMA" \
            --arg mode "probe_processes" \
            --arg status "$status" \
            --arg required_remote_wrapper "$REQUIRED_REMOTE_WRAPPER" \
            --argjson count "$count" \
            --argjson processes "$processes_json" \
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
                command:.command,
                reason:.reason
            })) as $detected |
            {
                schema:$schema,
                mode:$mode,
                status:$status,
                count:$count,
                processes:$processes,
                disk_pressure_context:$disk_context,
                localBuildPolicy:{
                    policy:"rch_only",
                    status:(if $count > 0 then "blocked" else "satisfied" end),
                    commandScope:"active_process_scan",
                    allowedReadOnlyCargoSubcommands:["metadata","locate-project","pkgid","tree"]
                },
                requiredRemoteWrapper:$required_remote_wrapper,
                detectedLocalBuilds:$detected,
                repairActions:(
                    if $count > 0 then
                        [{
                            priority:1,
                            kind:"inspect_shell_without_killing",
                            command:null,
                            message:"Inspect the reported process owner and command; this detector never kills or cleans up processes."
                        }]
                    else [] end
                ),
                evidence:[{
                    kind:"active_process_scan",
                    result:$status,
                    processCount:$count,
                    diskPressureContext:$disk_context
                }]
            }'
    else
        printf '{"schema":"%s","mode":"probe_processes","status":"%s","count":%d,"processes":[]}\n' \
            "$REPORT_SCHEMA" "$status" "$count"
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
        PS_FIXTURE="tests/fixtures/rch_local_cargo_tripwire/process_scan_ps_fixture.txt"
        PACKAGE_CACHE_PIDS_FIXTURE="102"
        fixture_body=$(probe_processes | sort -n -k1,1)
        fixture_count=$(printf '%s' "$fixture_body" | grep -c . || true)
        fixture_report=$(emit_json_probe "$fixture_body" "$fixture_count")
        PS_FIXTURE="$old_ps_fixture"
        PACKAGE_CACHE_PIDS_FIXTURE="$old_package_cache_pids_fixture"
        if ! printf '%s' "$fixture_report" | jq -e '
            .count == 3
            and ([.processes[].command] | map(contains("lsd")) | any | not)
            and ([.processes[].command] | map(contains("bash -s --")) | any | not)
            and ([.processes[].command] | map(contains("ssh -i")) | any | not)
            and any(.detectedLocalBuilds[]; .policyStatus == "local_cargo_read_only_lock_holder" and .subcommand == "metadata" and .packageCacheLockHeld == true)
            and any(.detectedLocalBuilds[]; .policyStatus == "local_cargo_disallowed" and .subcommand == "test" and .manifestPath == "/Users/jemanuel/projects/eidetic_engine_cli/Cargo.toml")
            and any(.detectedLocalBuilds[]; .policyStatus == "local_rust_tool_disallowed" and .commandKind == "rustc")
        ' >/dev/null; then
            printf 'self-test FAILED: process scan fixture did not produce expected classifications; got %s\n' "$fixture_report" >&2
            exit 1
        fi
    fi
    printf 'self-test PASSED: 21 classifier cases, JSON repair action, stable-wrapper/ssh exclusion, and process fixture produced expected outcomes\n'
    exit 0
}

if [ "$SELF_TEST" = true ]; then
    run_self_test
fi

case "$MODE" in
    cmd_classify)
        # An explicit `--cmd ""` is treated as a classifier query for the
        # empty command and returns allowed (the classifier already handles
        # empty input). Only complain when --cmd was never passed at all,
        # which is detectable here only via $MODE staying at the default
        # AND no positional fallback being supplied. For practical use,
        # the harness always passes --cmd, so allow the empty-string path
        # to flow through classify_command rather than hard-fail.
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
            COUNT=$(printf '%s' "$BODY" | grep -c . || true)
        else
            COUNT=0
        fi
        if [ "$JSON_OUTPUT" = true ]; then
            emit_json_probe "$BODY" "$COUNT"
        else
            emit_human_probe "$BODY" "$COUNT"
        fi
        if [ "$COUNT" -gt 0 ]; then exit 1; fi
        exit 0
        ;;
esac
