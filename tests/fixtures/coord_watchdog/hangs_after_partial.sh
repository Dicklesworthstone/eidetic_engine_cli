#!/usr/bin/env bash
# bd-12v87.5 — coord-watchdog fixture: command that emits one record
# then hangs.
#
# Emits one valid JSONL line on stdout, flushes, then sleeps for an hour
# (well past any reasonable source_run timeout). The runner must
# terminate the child within the configured timeout, retain the partial
# stdout tail, and report status=TimedOut with killed_own_child=true.
# Critically: the runner must NOT kill any peer process; only the
# process tree it started via Command::spawn.
set -eu
printf '{"schema":"ee.coord_watchdog.fixture.v1","scenario":"hangs_after_partial","record":1}\n'
# Flush stdout before sleeping so the partial tail is observable on the
# parent side regardless of how the runner consumes stdout (buffered
# read or streaming read).
exec 1>&-
sleep 3600
