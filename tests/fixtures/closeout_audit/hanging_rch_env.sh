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
