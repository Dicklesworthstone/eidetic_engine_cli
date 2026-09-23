#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/doctor_fixtures/lib.sh
. "$SCRIPT_DIR/../lib.sh"

# NOT-DETECTED, PINNED GAP (bd-2oh15 ruling c9986; FM-AC-01 declares
# fix_agent_coordination_stale_lease, but no doctor check reaches it): the
# workspace's Agent Mail snapshot (.ee/agent-mail-snapshot.json, a declared
# ee.agent_mail.snapshot.v1 that passes the strict validator) still lists an
# EXCLUSIVE reservation on a dirty file whose expires_ts is long past. ee's own
# coordination overlay classifies that lease as expired; doctor reads nothing.
FM="fm-agent_coordination-mcp-agent-mail-file-reservation-conflict"
target="$(doctor_fixture_target)"
ee_bin="${EE_DOCTOR_FIXTURE_BINARY:-ee}"
command -v "$ee_bin" >/dev/null
command -v git >/dev/null
export EE_EMBED_DOWNLOAD="${EE_EMBED_DOWNLOAD:-off}"
doctor_fixture_prepare_target "$target"
doctor_fixture_healthy_store "$FM" "$target" "$ee_bin"
base="$target/.fixture_baseline"
leased="src/leased.rs"
expired="2026-01-01T00:00:00Z"

# A checkout with one committed file that is then modified, so workspace
# hygiene has a dirty path for the reservation to match.
mkdir -p "$target/src"
printf 'pub fn leased() {}\n' > "$target/$leased"
git -C "$target" init -q
git -C "$target" add -- "$leased"
git -C "$target" -c user.name=doctor-fixture -c user.email=doctor-fixture@invalid \
    commit -q -m "doctor fixture baseline"
printf 'pub fn leased() -> u8 { 1 }\n' > "$target/$leased"

# The declared v1 snapshot: six ordered source commands sharing the "am"
# prefix, index-matched statuses, project_key bound to the canonical workspace
# path, and no degradation. Successful health probes must also report
# health_level and durability_state: the strict validator rejects a snapshot
# without them ("successful readiness probe omitted health_level", measured
# on 02524267e), which the shape of tests/agent_mail_fixture/snapshot_v1.rs
# omits.
snapshot() {
    local expires_ts="${1:?expires_ts required}"
    local canonical key
    canonical="$(cd "$target" && pwd -P)"
    key="sha256:$(printf '%s' "$canonical" | shasum -a 256 | cut -d' ' -f1)"
    jq -n --arg key "$key" --arg now "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        --arg path "$leased" --arg expires "$expires_ts" '
        "DoctorFixture" as $agent |
        [
            "am agents list --project '"'"'<workspace>'"'"' --json",
            "am robot reservations --project '"'"'<workspace>'"'"' --all --format json",
            "am mail inbox --project '"'"'<workspace>'"'"' --agent \($agent) --limit 50 --json",
            "am status --project '"'"'<workspace>'"'"' --agent \($agent) --json",
            "agent-mail-health http://127.0.0.1:8765/health",
            "agent-mail-health http://127.0.0.1:8765/health/durability"
        ] as $commands |
        {
            schema: "ee.agent_mail.snapshot.v1",
            generated_at: $now,
            project_key: $key,
            agent_name: $agent,
            redaction_status: "paths_counts_subjects_only_no_content",
            producer_status: "ok",
            source_commands: $commands,
            command_statuses: [$commands | to_entries[] | {
                command: .value, ok: true,
                exit_code: (if .key < 4 then 0 else 200 end),
                timed_out: false, error_class: null
            }],
            fallback_active: false,
            am_agents_list_ok: true,
            health_level: "green",
            durability_state: "ok",
            summary: {
                agent_count: 1, file_reservation_count: 1, inbox_mailbox_count: 1,
                thread_count: 0, source_command_count: 6, degraded_count: 0
            },
            degraded: [],
            file_reservations: [{
                path_pattern: $path, holder: "StaleHolder", exclusive: true, expires_ts: $expires
            }],
            agents: [{name: $agent}],
            inbox: [{mailbox: $agent, unread_count: 0, ack_required_count: 0}],
            threads: []
        }'
}
hygiene() {
    (cd "$target" && "$ee_bin" --workspace "$target" --json workspace hygiene \
        --agent-name DoctorFixture --agent-mail-snapshot "$1")
}

# Negative control: the same lease with a future expiry is live, and the
# overlay blocks the dirty path on it. This proves the snapshot is accepted
# and the pattern matches, so the expired reading below is not vacuous.
snapshot "2099-01-01T00:00:00Z" > "$base/agent-mail-snapshot.live.json"
hygiene "$base/agent-mail-snapshot.live.json" > "$base/hygiene-live.json"
jq -e --arg path "$leased" '
    .schema == "ee.response.v2" and .success == true and
    .data.coordinationState.agentMailAvailable == true and
    any(.data.coordinationState.blockedByCoordination[];
        .path == $path and .holderAgent == "StaleHolder" and
        (.reasons | index("active_exclusive_reservation")))
' "$base/hygiene-live.json" >/dev/null

snapshot "$expired" > "$target/.ee/agent-mail-snapshot.json"
hygiene "$target/.ee/agent-mail-snapshot.json" > "$base/hygiene-expired.json"
if ! jq -e --arg path "$leased" '
    .schema == "ee.response.v2" and .success == true and
    .data.coordinationState.agentMailAvailable == true and
    (.data.coordinationState.blockedByCoordination | length == 0) and
    any(.data.coordinationState.ignoredReservations[];
        .path == $path and .holderAgent == "StaleHolder" and .exclusive == true and
        (.reasons | index("expired_reservation_ignored")))
' "$base/hygiene-expired.json" >/dev/null; then
    printf 'agent-mail fixture: hygiene did not classify the lease as expired; see %s\n' \
        "$base/hygiene-expired.json" >&2
    exit 1
fi
doctor_fixture_corrupt "$FM" "P1" "agent_coordination"
printf 'real stale lease confirmed: exclusive reservation on %s expired %s (expired_reservation_ignored)\n' \
    "$leased" "$expired" >&2
