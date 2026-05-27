#!/usr/bin/env bash
# bd-12v87.5 — coord-watchdog fixture: MALFORMED JSONL response.
#
# Outputs a deliberately-broken JSONL line (missing closing brace) on
# stdout. The source_run runner itself does not parse JSON, so the
# command status is Passed; downstream consumers must classify the
# stdout body as parse-failed and emit their own degraded code. This
# fixture lets the e2e assert that the runner reports the exact stdout
# tail unchanged so consumers can detect the malformed shape.
set -eu
printf '{"schema":"ee.coord_watchdog.fixture.v1","scenario":"malformed_jsonl","record":1\n'
exit 0
