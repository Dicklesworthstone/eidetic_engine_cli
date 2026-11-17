#!/usr/bin/env bash
# scripts/e2e_orphan_partition_selftest.sh — compatibility wrapper delegating to
# scripts/audit_e2e_orphan_partition_selftest.sh (bd-1ult2).
#
# Retained in-place (per AGENTS.md RULE 1 no-deletion policy) while routing all
# invocations directly to the canonical audit_e2e_orphan_partition_selftest.sh
# tool and excluded from the e2e suite population in scripts/e2e_invocation_audit.sh.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/audit_e2e_orphan_partition_selftest.sh" "$@"
