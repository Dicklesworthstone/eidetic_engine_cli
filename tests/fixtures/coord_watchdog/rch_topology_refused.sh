#!/usr/bin/env bash
# bd-12v87.5 — coord-watchdog fixture: RCH verifier topology refusal.
#
# Simulates the well-known RCH-E327 path-dependency topology refusal
# documented at bd-17c65.10.17.1.2. Emits the canonical refusal text to
# stderr and exits with the structured RCH refusal code (rch_verify
# uses exit 1 for environment failures). The runner must surface the
# stderr tail and exit code so downstream consumers can detect the
# topology blocker without retrying. Used by both the watchdog
# integration and by anything that drives the rch_verify wrapper.
set -eu
printf '[RCH] local (dependency preflight RCH-E327: Path dependency topology policy failed; move dependencies under /data/projects (or /dp) and retry.)\n' >&2
printf '[RCH] remote required; refusing local fallback (dependency preflight failed)\n' >&2
exit 1
