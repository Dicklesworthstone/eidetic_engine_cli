#!/usr/bin/env bash
# bd-12v87.5 — coord-watchdog fixture: Agent Mail health corruption.
#
# Simulates the well-known Agent Mail failure mode where the underlying
# SQLite store is corrupt: emits an error line to stderr matching the
# canonical "malformed disk image" message and exits non-zero. The
# source_run runner should report status=Failed with the exit code
# preserved, the stderr tail retained for diagnosis, and no mutation.
# Downstream surfaces (swarm_brief, work-packet) then classify this as
# agent_mail_semantic_readiness_failed per
# tests/fixtures/failure_modes/agent_mail_semantic_readiness_failed.json.
set -eu
printf 'Agent Mail: database is malformed (disk I/O error: malformed disk image)\n' >&2
exit 21
