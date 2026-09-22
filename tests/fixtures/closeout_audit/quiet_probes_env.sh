# BASH_ENV stand-ins for the closeout audit's live host probes (bd-f5j1x).
#
# The readiness fixtures grade the fixture, not the machine running them. The
# audit script also consults the host -- `br dep cycles` under an 8s bound, `rch
# check` and `rch queue`, an HTTP health check against Agent Mail, and `git
# status` of whatever repository encloses the fixture -- and a slow or busy host
# turned a nominal `ready` into `blocked` or `ready_with_caveats`. Shell functions
# shadow executables on PATH, and bash re-reads BASH_ENV in every non-interactive
# child, so these also reach the script's `bash -c` subshells.
#
# Each stand-in takes the script's quiet, deterministic branch:
#   br    exits 1 (not a timeout)  -> the script's own JSONL cycle scan of the fixture
#   rch   check ok, queue empty    -> rch ready, queue "unavailable" (no caveat)
#   curl  succeeds                 -> Agent Mail reachable (no caveat)
#   git   status prints nothing    -> no uncommitted files reference the bead
#
# Probe-specific fixtures (hanging_*_env.sh) source this first and then replace
# only the probe they exercise.

br() {
    return 1
}

rch() {
    case "${1:-}" in
        check)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

curl() {
    return 0
}

git() {
    case "${1:-}" in
        status)
            return 0
            ;;
        *)
            command git "$@"
            ;;
    esac
}
