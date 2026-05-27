#!/usr/bin/env bash
# bd-12v87.5 — coord-watchdog fixture: CLEAN Beads/BV source response.
#
# Outputs a single well-formed JSONL record on stdout and exits 0. The
# source_run runner should classify this as SourceRunStatus::Passed with
# no degraded entries.
set -eu
printf '{"schema":"ee.coord_watchdog.fixture.v1","scenario":"clean","record":1}\n'
exit 0
