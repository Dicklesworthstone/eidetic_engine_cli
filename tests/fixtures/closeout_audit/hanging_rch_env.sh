# Every other host probe stays quiet, so only the hung RCH probe can move the
# verdict (bd-f5j1x).
source "$(dirname "${BASH_SOURCE[0]}")/quiet_probes_env.sh"

rch() {
    case "${1:-}" in
        check|queue)
            sleep 10
            ;;
        *)
            return 1
            ;;
    esac
}
