__EE_CLOSEOUT_AUDIT_REAL_JQ="$(command -v jq)"

br() {
    case "${1:-} ${2:-}" in
        "dep cycles")
            sleep 10
            return 124
            ;;
        *)
            return 1
            ;;
    esac
}

jq() {
    for arg in "$@"; do
        case "$arg" in
            *dependency_targets*)
                sleep 10
                return 124
                ;;
        esac
    done
    "$__EE_CLOSEOUT_AUDIT_REAL_JQ" "$@"
}
