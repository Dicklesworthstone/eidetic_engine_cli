#!/usr/bin/env bash
#
# ee (Eidetic Engine CLI) installer
# Durable, local-first, explainable memory for coding agents.
#
# One-liner install (cache-busted):
#   curl -fsSL "https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/latest/download/install.sh?$(date +%s)" | bash
#
# Pinned version:
#   curl -fsSL https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.sh | EE_VERSION=v0.1.0 bash
#
# Options:
#   --version vX.Y.Z   Install specific version (default: latest)
#   --dest DIR         Install to DIR (default: ~/.local/bin)
#   --system           Install to /usr/local/bin (requires sudo)
#   --easy-mode        Auto-update PATH in ~/.zshrc/~/.bashrc
#   --verify           Run `ee --version` and `ee doctor --json` self-test
#   --from-source      Build from source instead of downloading binary
#   --quiet, -q        Suppress non-error output
#   --offline          Skip network preflight checks (use with --artifact-url)
#   --no-gum           Disable gum formatting even if available
#   --no-configure     Skip agent integration instructions (still installs binary)
#   --no-verify        Skip checksum + Sigstore verification (NOT recommended)
#   --force            Force reinstall even when same version is already present
#
# Environment variables (legacy names honored for back-compat):
#   EE_VERSION         specific version to install (== --version)
#   EE_INSTALL_DIR     installation directory (== --dest)
#   EE_SKIP_VERIFY     set to 1 to skip verification (== --no-verify)
#   HTTPS_PROXY / HTTP_PROXY   honored for every network call
#
set -euo pipefail
umask 022
shopt -s lastpipe 2>/dev/null || true

# ───────────────────────────────────────────────────────────────────────────
# Defaults & CLI state
# ───────────────────────────────────────────────────────────────────────────

OWNER="${OWNER:-Dicklesworthstone}"
REPO="${REPO:-eidetic_engine_cli}"
BINARY="ee"
PROJECT_LABEL="ee (Eidetic Engine)"
DEST_DEFAULT="$HOME/.local/bin"

VERSION="${EE_VERSION:-${VERSION:-}}"
DEST="${EE_INSTALL_DIR:-${DEST:-$DEST_DEFAULT}}"
ARTIFACT_URL="${ARTIFACT_URL:-}"
CHECKSUM="${CHECKSUM:-}"
CHECKSUM_URL="${CHECKSUM_URL:-}"

# Sigstore trust anchors are part of the installer security boundary. Do not
# honor environment overrides here; curl-pipe-bash callers can set env inline.
CERT_IDENTITY_REGEXP="^https://github\.com/${OWNER}/${REPO}/\.github/workflows/release\.yml@refs/(tags/v[0-9].*|heads/main)$"
CERT_OIDC_ISSUER="https://token.actions.githubusercontent.com"

EASY=0
QUIET=0
VERIFY=0
FROM_SOURCE=0
SYSTEM=0
NO_GUM=0
NO_CONFIGURE=0
NO_CHECKSUM="${EE_SKIP_VERIFY:-0}"
FORCE_INSTALL=0
OFFLINE="${EE_OFFLINE:-0}"

LOCK_FILE="/tmp/ee-install.lock"
AGENT_VERSION_LOOKUP="${EE_INSTALLER_AGENT_VERSIONS:-1}"
AGENT_VERSION_TIMEOUT="${EE_INSTALLER_AGENT_VERSION_TIMEOUT:-1}"

# ───────────────────────────────────────────────────────────────────────────
# Output stack: gum-aware logging with ANSI fallback
# ───────────────────────────────────────────────────────────────────────────

HAS_GUM=0
if command -v gum &>/dev/null && [ -t 1 ]; then
  HAS_GUM=1
fi

log() { [ "$QUIET" -eq 1 ] && return 0; echo -e "$@"; }

info() {
  [ "$QUIET" -eq 1 ] && return 0
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    gum style --foreground 39 "→ $*"
  else
    echo -e "\033[0;34m→\033[0m $*"
  fi
}

ok() {
  [ "$QUIET" -eq 1 ] && return 0
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    gum style --foreground 42 "✓ $*"
  else
    echo -e "\033[0;32m✓\033[0m $*"
  fi
}

warn() {
  [ "$QUIET" -eq 1 ] && return 0
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    gum style --foreground 214 "⚠ $*" >&2
  else
    echo -e "\033[1;33m⚠\033[0m $*" >&2
  fi
}

err() {
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    gum style --foreground 196 "✗ $*" >&2
  else
    echo -e "\033[0;31m✗\033[0m $*" >&2
  fi
}

run_with_spinner() {
  local title="$1"
  shift
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ] && [ "$QUIET" -eq 0 ]; then
    gum spin --spinner dot --title "$title" -- "$@"
  else
    info "$title"
    "$@"
  fi
}

# Draw a colored box with automatic width handling (ANSI-aware).
# Usage: draw_box "color_code" "line1" "line2" ...
draw_box() {
  local color="$1"
  shift
  local lines=("$@")
  local max_width=0
  local esc
  esc=$(printf '\033')
  local strip_ansi_sed="s/${esc}\\[[0-9;]*m//g"

  for line in "${lines[@]}"; do
    local stripped
    stripped=$(printf '%b' "$line" | LC_ALL=C sed "$strip_ansi_sed")
    local len=${#stripped}
    if [ "$len" -gt "$max_width" ]; then
      max_width=$len
    fi
  done

  local inner_width=$((max_width + 4))
  local border=""
  local i
  for ((i=0; i<inner_width; i++)); do
    border+="═"
  done

  printf "\033[%sm╔%s╗\033[0m\n" "$color" "$border"
  for line in "${lines[@]}"; do
    local stripped
    stripped=$(printf '%b' "$line" | LC_ALL=C sed "$strip_ansi_sed")
    local len=${#stripped}
    local padding=$((max_width - len))
    local pad_str=""
    for ((i=0; i<padding; i++)); do
      pad_str+=" "
    done
    printf "\033[%sm║\033[0m  %b%s  \033[%sm║\033[0m\n" "$color" "$line" "$pad_str" "$color"
  done
  printf "\033[%sm╚%s╝\033[0m\n" "$color" "$border"
}

# ───────────────────────────────────────────────────────────────────────────
# Proxy
# ───────────────────────────────────────────────────────────────────────────

PROXY_ARGS=()
setup_proxy() {
  PROXY_ARGS=()
  if [[ -n "${HTTPS_PROXY:-}" ]]; then
    PROXY_ARGS=(--proxy "$HTTPS_PROXY")
  elif [[ -n "${HTTP_PROXY:-}" ]]; then
    PROXY_ARGS=(--proxy "$HTTP_PROXY")
  fi
}

# Curl wrapper that honors proxy and standard timeouts. Treat as `curl -fsSL`
# with proxy + retry pre-wired.
ee_curl() {
  curl -fsSL --retry 2 --retry-delay 1 "${PROXY_ARGS[@]}" "$@"
}

# ───────────────────────────────────────────────────────────────────────────
# Usage
# ───────────────────────────────────────────────────────────────────────────

usage() {
  cat <<EOFU
Usage: install.sh [--version vX.Y.Z] [--dest DIR] [--system] [--easy-mode] [--verify] \\
                  [--artifact-url URL] [--checksum HEX] [--checksum-url URL] \\
                  [--from-source] [--quiet] [--offline] [--no-gum] [--no-configure] \\
                  [--no-verify] [--force]

Options:
  --version vX.Y.Z   Install specific version (default: latest GitHub release)
  --dest DIR         Install to DIR (default: ~/.local/bin)
  --system           Install to /usr/local/bin (requires write permission)
  --easy-mode        Auto-update PATH in ~/.zshrc and ~/.bashrc
  --verify           Run \`ee --version\` and \`ee doctor --json\` after install
  --artifact-url URL Override the tarball download URL
  --checksum HEX     Use this SHA256 instead of fetching <url>.sha256
  --checksum-url URL Fetch SHA256 from this URL instead of <url>.sha256
  --from-source      Build from source via git+cargo instead of downloading
  --quiet, -q        Suppress non-error output
  --offline          Skip network preflight checks
  --no-gum           Disable gum formatting even if available
  --no-configure     Skip agent integration instructions (still installs binary)
  --no-verify        Skip SHA256 + Sigstore verification (NOT recommended)
  --force            Reinstall even when the same version is already present

Environment variables:
  EE_VERSION         == --version
  EE_INSTALL_DIR     == --dest
  EE_SKIP_VERIFY=1   == --no-verify
  HTTPS_PROXY        Proxy URL honored on every curl call
EOFU
}

require_option_value() {
  local option="$1"
  local value="${2:-}"
  if [ -z "$value" ] || [[ "$value" == -* ]]; then
    err "$option requires a value"
    usage
    exit 2
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) require_option_value "$1" "${2:-}"; VERSION="$2"; shift 2;;
    --dest) require_option_value "$1" "${2:-}"; DEST="$2"; shift 2;;
    --system) SYSTEM=1; DEST="/usr/local/bin"; shift;;
    --easy-mode) EASY=1; shift;;
    --verify) VERIFY=1; shift;;
    --artifact-url) require_option_value "$1" "${2:-}"; ARTIFACT_URL="$2"; shift 2;;
    --checksum) require_option_value "$1" "${2:-}"; CHECKSUM="$2"; shift 2;;
    --checksum-url) require_option_value "$1" "${2:-}"; CHECKSUM_URL="$2"; shift 2;;
    --from-source) FROM_SOURCE=1; shift;;
    --quiet|-q) QUIET=1; shift;;
    --offline) OFFLINE=1; shift;;
    --no-gum) NO_GUM=1; shift;;
    --no-configure) NO_CONFIGURE=1; shift;;
    --no-verify) NO_CHECKSUM=1; shift;;
    --force) FORCE_INSTALL=1; shift;;
    -h|--help) usage; exit 0;;
    *) err "Unknown option: $1"; usage; exit 2;;
  esac
done

# Normalize the legacy "v"-less form: --version 0.1.0 → v0.1.0.
if [ -n "$VERSION" ]; then
  case "$VERSION" in
    v*) ;;
    *) VERSION="v${VERSION}" ;;
  esac
fi

setup_proxy

# ───────────────────────────────────────────────────────────────────────────
# Agent detection
# ───────────────────────────────────────────────────────────────────────────

DETECTED_AGENTS=()
CLAUDE_VERSION=""
CODEX_VERSION=""
GEMINI_VERSION=""
AIDER_VERSION=""
CURSOR_VERSION=""
COPILOT_VERSION=""
CONTINUE_VERSION=""

try_version() {
  local cmd="$1"
  [[ "$AGENT_VERSION_LOOKUP" == "1" ]] || return 0
  command -v "$cmd" >/dev/null 2>&1 || return 0
  local t="${AGENT_VERSION_TIMEOUT:-1}"
  [[ "$t" =~ ^[0-9]+$ ]] || t=1
  if command -v timeout >/dev/null 2>&1; then
    timeout "$t" "$cmd" --version 2>/dev/null | head -1 || true
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$t" "$cmd" --version 2>/dev/null | head -1 || true
  else
    "$cmd" --version 2>/dev/null | head -1 || true
  fi
}

print_agent_scan_notice() {
  [ "$QUIET" -eq 1 ] && return 0
  local l1="Scanning for installed coding agents…"
  local l2="(this is informational; no agent settings will be modified)"
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    echo ""
    gum style --border normal --border-foreground 244 --padding "0 1" \
      "$(gum style --foreground 212 --bold 'Agent scan')" \
      "$(gum style --foreground 247 "$l1")" \
      "$(gum style --foreground 245 "$l2")"
    echo ""
  else
    echo ""
    draw_box "0;36" "$l1" "$l2"
    echo ""
  fi
}

detect_agents() {
  DETECTED_AGENTS=()

  if [[ -d "$HOME/.claude" ]] || command -v claude &>/dev/null; then
    DETECTED_AGENTS+=("claude-code")
    CLAUDE_VERSION=$(try_version claude)
  fi

  if [[ -d "$HOME/.codex" ]] || command -v codex &>/dev/null; then
    DETECTED_AGENTS+=("codex-cli")
    CODEX_VERSION=$(try_version codex)
  fi

  if [[ -d "$HOME/.gemini" ]] || [[ -d "$HOME/.gemini-cli" ]] || command -v gemini &>/dev/null; then
    DETECTED_AGENTS+=("gemini-cli")
    GEMINI_VERSION=$(try_version gemini)
  fi

  if command -v aider &>/dev/null; then
    DETECTED_AGENTS+=("aider")
    AIDER_VERSION=$(try_version aider)
  fi

  if command -v copilot &>/dev/null || [[ -d "$HOME/.copilot" ]]; then
    DETECTED_AGENTS+=("github-copilot-cli")
    COPILOT_VERSION=$(try_version copilot)
  fi

  if [[ -d "$HOME/.continue" ]]; then
    DETECTED_AGENTS+=("continue")
    [[ -f "$HOME/.continue/config.json" ]] && CONTINUE_VERSION="config present"
  fi

  local cursor_detected=0
  local cursor_mac="$HOME/Library/Application Support/Cursor/User/settings.json"
  local cursor_linux="$HOME/.config/Cursor/User/settings.json"
  if [[ -d "$HOME/.cursor" ]] || [[ -f "$cursor_mac" ]] || [[ -f "$cursor_linux" ]] || command -v cursor &>/dev/null; then
    cursor_detected=1
  fi
  if [ "$cursor_detected" -eq 1 ]; then
    DETECTED_AGENTS+=("cursor-ide")
    CURSOR_VERSION=$(try_version cursor)
  fi
}

print_detected_agents() {
  [ "$QUIET" -eq 1 ] && return 0
  if [[ ${#DETECTED_AGENTS[@]} -eq 0 ]]; then
    info "No AI coding agents detected"
    return
  fi
  local count=${#DETECTED_AGENTS[@]}
  local plural=""; [[ $count -gt 1 ]] && plural="s"

  # Inner helper. `local` cannot scope a function definition in bash, so
  # `render` is defined globally; defining it here keeps it co-located with
  # its only caller below.
  render() {
    local label="$1" ver="$2"
    local v=""
    [[ -n "$ver" ]] && v=" (${ver})"
    if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
      gum style --foreground 42 "  ✓ ${label}${v}"
    else
      echo -e "  \033[0;32m✓\033[0m ${label}${v}"
    fi
  }

  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    echo ""
    gum style --foreground 39 --bold "Detected AI Coding Agent${plural}:"
  else
    echo ""
    echo -e "\033[1;39mDetected AI Coding Agent${plural}:\033[0m"
  fi
  for agent in "${DETECTED_AGENTS[@]}"; do
    case "$agent" in
      claude-code)        render "Claude Code"        "$CLAUDE_VERSION" ;;
      codex-cli)          render "Codex CLI"          "$CODEX_VERSION" ;;
      gemini-cli)         render "Gemini CLI"         "$GEMINI_VERSION" ;;
      aider)              render "Aider"              "$AIDER_VERSION" ;;
      github-copilot-cli) render "GitHub Copilot CLI" "$COPILOT_VERSION" ;;
      continue)           render "Continue"           "$CONTINUE_VERSION" ;;
      cursor-ide)         render "Cursor IDE"         "$CURSOR_VERSION" ;;
    esac
  done
  echo ""
}

is_agent_detected() {
  local target="$1"
  for agent in "${DETECTED_AGENTS[@]}"; do
    [[ "$agent" == "$target" ]] && return 0
  done
  return 1
}

# ───────────────────────────────────────────────────────────────────────────
# Platform detection
# ───────────────────────────────────────────────────────────────────────────

OS=""
ARCH=""
TARGET=""

detect_platform() {
  OS=$(uname -s | tr 'A-Z' 'a-z')
  ARCH=$(uname -m)
  case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    arm64|aarch64) ARCH="aarch64" ;;
    *) warn "Unknown arch $ARCH; will fall back to source build if no prebuilt artifact matches" ;;
  esac

  # WSL gets reported as linux but document the caveat.
  if [ "$OS" = "linux" ] && [ -r /proc/version ] && grep -qi microsoft /proc/version 2>/dev/null; then
    warn "WSL detected — install proceeds with Linux artifacts; some POSIX path features may differ"
  fi

  case "${OS}-${ARCH}" in
    linux-x86_64)
      # Release pipeline builds both gnu and musl; musl is statically linked
      # and works on every glibc generation we care about. Prefer musl.
      TARGET="x86_64-unknown-linux-musl"
      ;;
    linux-aarch64)
      # Release pipeline ships only gnu for aarch64-linux.
      TARGET="aarch64-unknown-linux-gnu"
      ;;
    darwin-x86_64)  TARGET="x86_64-apple-darwin" ;;
    darwin-aarch64) TARGET="aarch64-apple-darwin" ;;
    *) TARGET="" ;;
  esac

  if [ -z "$TARGET" ] && [ "$FROM_SOURCE" -eq 0 ] && [ -z "$ARTIFACT_URL" ]; then
    warn "No prebuilt artifact for ${OS}/${ARCH}; will build from source"
    FROM_SOURCE=1
  fi

  info "Platform: ${OS}/${ARCH}${TARGET:+ → $TARGET}"
}

# ───────────────────────────────────────────────────────────────────────────
# Version resolution and artifact URL
# ───────────────────────────────────────────────────────────────────────────

resolve_version() {
  if [ -n "$VERSION" ]; then return 0; fi
  if [ "$FROM_SOURCE" -eq 1 ] || [ -n "$ARTIFACT_URL" ]; then return 0; fi

  info "Resolving latest version…"
  local api="https://api.github.com/repos/${OWNER}/${REPO}/releases/latest"
  local tag=""
  tag=$(ee_curl -H "Accept: application/vnd.github.v3+json" "$api" 2>/dev/null \
        | grep '"tag_name":' | head -1 \
        | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/') || tag=""

  if [ -z "$tag" ]; then
    # Redirect fallback: /releases/latest -> /releases/tag/vX.Y.Z
    local redirect="https://github.com/${OWNER}/${REPO}/releases/latest"
    tag=$(ee_curl -o /dev/null -w '%{url_effective}' "$redirect" 2>/dev/null \
          | sed -E 's|.*/tag/||') || tag=""
    if ! [[ "$tag" =~ ^v[0-9] ]] || [[ "$tag" == *"/"* ]]; then
      tag=""
    fi
  fi

  if [ -z "$tag" ]; then
    err "Could not resolve latest release."
    err "Re-run with --version vX.Y.Z, --artifact-url URL, or --from-source."
    exit 1
  fi

  VERSION="$tag"
  info "Resolved latest version: $VERSION"
}

TAR=""
URL=""

set_artifact_url() {
  TAR=""
  URL=""
  if [ "$FROM_SOURCE" -eq 1 ]; then
    return 0
  fi
  if [ -n "$ARTIFACT_URL" ]; then
    URL="$ARTIFACT_URL"
    TAR="$(basename "$ARTIFACT_URL")"
    return 0
  fi
  if [ -z "$TARGET" ]; then
    FROM_SOURCE=1
    return 0
  fi

  TAR="ee-${TARGET}.tar.xz"
  URL="https://github.com/${OWNER}/${REPO}/releases/download/${VERSION}/${TAR}"
}

# ───────────────────────────────────────────────────────────────────────────
# Preflight checks
# ───────────────────────────────────────────────────────────────────────────

check_disk_space() {
  local min_kb=20480  # 20 MB headroom
  local path="$DEST"
  [ -d "$path" ] || path=$(dirname "$path")
  if command -v df >/dev/null 2>&1; then
    local avail
    avail=$(df -Pk "$path" 2>/dev/null | awk 'NR==2 {print $4}') || avail=""
    if [ -n "$avail" ] && [ "$avail" -lt "$min_kb" ]; then
      err "Insufficient disk space in $path (need ≥ ${min_kb}KB, have ${avail}KB)"
      exit 1
    fi
  else
    warn "df not found; skipping disk space check"
  fi
}

check_write_permissions() {
  if [ ! -d "$DEST" ]; then
    if ! mkdir -p "$DEST" 2>/dev/null; then
      err "Cannot create $DEST (insufficient permissions)"
      err "Try a writable --dest, or re-run with sudo for --system"
      exit 1
    fi
  fi
  if [ ! -w "$DEST" ]; then
    err "No write permission to $DEST"
    err "Try a writable --dest, or re-run with sudo for --system"
    exit 1
  fi
}

check_existing_install() {
  if [ -x "$DEST/$BINARY" ]; then
    local current
    current=$("$DEST/$BINARY" --version 2>/dev/null | head -1 || echo "")
    if [ -n "$current" ]; then
      info "Existing ${BINARY} detected: $current"
    fi
  fi
}

check_network() {
  [ "$OFFLINE" -eq 1 ] && { info "Offline mode; skipping network preflight"; return 0; }
  [ "$FROM_SOURCE" -eq 1 ] && return 0
  [ -z "$URL" ] && return 0
  command -v curl >/dev/null 2>&1 || { warn "curl not found; skipping network check"; return 0; }
  if ! ee_curl --connect-timeout 3 --max-time 5 -o /dev/null "$URL" 2>/dev/null; then
    warn "Network check failed for $URL — continuing; download may fail"
  fi
}

preflight_checks() {
  info "Running preflight checks"
  check_disk_space
  check_write_permissions
  check_existing_install
  check_network
}

# ───────────────────────────────────────────────────────────────────────────
# Already-installed detection
# ───────────────────────────────────────────────────────────────────────────

check_installed_version() {
  local target="$1"
  [ -x "$DEST/$BINARY" ] || return 1
  local installed
  # BSD sed (macOS) treats `\+` as literal `+`, not the GNU "one or more"
  # quantifier — so the prior regex silently failed to match on macOS,
  # making check_installed_version always return 1 (broken short-circuit,
  # benign re-install). Use portable POSIX BRE: `[[:space:]][[:space:]]*`.
  installed=$("$DEST/$BINARY" --version 2>/dev/null | head -1 \
    | sed -n -e 's/.*ee[[:space:]][[:space:]]*v\{0,1\}\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p' \
             -e 's/^[[:space:]]*v\{0,1\}\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)[[:space:]]*$/\1/p' \
    | head -1)
  [ -z "$installed" ] && return 1
  local clean_target="${target#v}"
  local clean_installed="${installed#v}"
  [ "$clean_target" = "$clean_installed" ]
}

# ───────────────────────────────────────────────────────────────────────────
# PATH and completions
# ───────────────────────────────────────────────────────────────────────────

maybe_add_path() {
  case ":$PATH:" in
    *:"$DEST":*) return 0 ;;
    *)
      if [ "$EASY" -eq 1 ]; then
        local appended=0
        local rc_existed=0
        for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
          if [ -e "$rc" ] && [ -w "$rc" ]; then
            rc_existed=1
            if ! grep -F "$DEST" "$rc" >/dev/null 2>&1; then
              echo "export PATH=\"$DEST:\$PATH\"" >> "$rc"
              appended=1
            fi
          fi
        done
        if [ "$appended" -eq 1 ]; then
          warn "PATH updated in ~/.zshrc/.bashrc — restart your shell to use $BINARY"
        elif [ "$rc_existed" -eq 1 ]; then
          info "PATH already configured in ~/.zshrc/.bashrc — restart your shell to use $BINARY"
        else
          warn "Add $DEST to PATH to use $BINARY (no writable ~/.zshrc or ~/.bashrc found)"
        fi
      else
        warn "Add $DEST to PATH to use $BINARY (or re-run with --easy-mode to auto-update)"
      fi
      ;;
  esac
}

detect_default_shell() {
  local shell="${SHELL:-}"
  [ -z "$shell" ] && return 1
  shell=$(basename "$shell")
  case "$shell" in
    bash|zsh|fish) echo "$shell"; return 0 ;;
    *) return 1 ;;
  esac
}

install_completions_for_shell() {
  local shell="$1"
  local bin="$DEST/$BINARY"
  if [ ! -x "$bin" ]; then
    warn "$BINARY binary not found at $bin; skipping completions"
    return 1
  fi
  # ee uses the singular subcommand `completion <shell>`.
  if ! "$bin" completion --help >/dev/null 2>&1; then
    info "Shell completions: skipped (not supported in this build)"
    return 0
  fi

  local target=""
  case "$shell" in
    bash) target="${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions/ee" ;;
    zsh)  target="${XDG_DATA_HOME:-$HOME/.local/share}/zsh/site-functions/_ee" ;;
    fish) target="${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions/ee.fish" ;;
    *) return 1 ;;
  esac

  if ! mkdir -p "$(dirname "$target")" 2>/dev/null; then
    warn "Failed to create completions directory for $shell"
    return 1
  fi
  if "$bin" completion "$shell" > "$target" 2>/dev/null; then
    ok "Installed $shell completions to $target"
    return 0
  fi
  warn "Failed to install $shell completions"
  return 1
}

maybe_install_completions() {
  local shell=""
  if ! shell=$(detect_default_shell); then
    info "Shell completions: skipped (unknown shell)"
    return 0
  fi
  install_completions_for_shell "$shell" || true
}

# ───────────────────────────────────────────────────────────────────────────
# Checksum + Sigstore
# ───────────────────────────────────────────────────────────────────────────

verify_checksum() {
  local file="$1" expected="$2" actual=""
  [ -f "$file" ] || { err "File not found: $file"; return 1; }

  if command -v sha256sum &>/dev/null; then
    actual=$(sha256sum "$file" | cut -d' ' -f1)
  elif command -v shasum &>/dev/null; then
    actual=$(shasum -a 256 "$file" | cut -d' ' -f1)
  else
    err "No SHA256 tool found. Install sha256sum or shasum, or set EE_SKIP_VERIFY=1 to bypass verification."
    return 1
  fi

  if [ "$actual" != "$expected" ]; then
    err "Checksum verification FAILED!"
    err "Expected: $expected"
    err "Got:      $actual"
    err "The downloaded file may be corrupted or tampered with."
    rm -f "$file"
    return 1
  fi
  ok "Checksum verified: ${actual:0:16}…"
}

verify_sigstore_bundle() {
  local file="$1" artifact_url="$2"
  if ! command -v cosign &>/dev/null; then
    err "cosign not found. Cannot verify Sigstore signature."
    err "Install cosign (https://github.com/sigstore/cosign) or use --no-verify to skip."
    return 1
  fi

  local bundle_url="${artifact_url}.sigstore.json"

  local bundle_file
  bundle_file="$TMP/$(basename "$bundle_url")"
  info "Fetching sigstore bundle"
  info "Sigstore bundle: $bundle_url" >&2
  info "Sigstore identity regexp: $CERT_IDENTITY_REGEXP" >&2
  info "Sigstore OIDC issuer: $CERT_OIDC_ISSUER" >&2
  if ! ee_curl "$bundle_url" -o "$bundle_file" 2>/dev/null; then
    err "Sigstore bundle not available at $bundle_url."
    err "Cannot verify signature. To skip cryptographic verification, use --no-verify or set EE_SKIP_VERIFY=1."
    return 1
  fi

  if ! cosign verify-blob \
        --bundle "$bundle_file" \
        --certificate-identity-regexp "$CERT_IDENTITY_REGEXP" \
        --certificate-oidc-issuer "$CERT_OIDC_ISSUER" \
        "$file" >/dev/null 2>&1; then
    err "Sigstore signature verification failed for $file"
    return 1
  fi
  ok "Sigstore signature verified"
}

# ───────────────────────────────────────────────────────────────────────────
# Build-from-source helpers
# ───────────────────────────────────────────────────────────────────────────

ensure_rust() {
  if [ "${RUSTUP_INIT_SKIP:-0}" != "0" ]; then
    info "Skipping rustup install (RUSTUP_INIT_SKIP set)"
    return 0
  fi
  if command -v cargo >/dev/null 2>&1 && rustc --version 2>/dev/null | grep -q nightly; then
    return 0
  fi
  if [ "$EASY" -ne 1 ] && [ -t 0 ]; then
    echo -n "ee requires Rust nightly. Install via rustup? (y/N): "
    # `read` returns non-zero on EOF; under `set -e` that would terminate the
    # script, so swallow the failure and let an empty answer fall through to
    # the default-deny branch.
    local ans=""
    read -r ans || ans=""
    case "$ans" in y|Y|yes|Yes) :;; *) warn "Skipping rustup install"; return 0;; esac
  fi
  info "Installing rustup (nightly toolchain)…"
  ee_curl https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly --profile minimal
  export PATH="$HOME/.cargo/bin:$PATH"
  rustup component add rustfmt clippy 2>/dev/null || true
}

# ───────────────────────────────────────────────────────────────────────────
# Header banner
# ───────────────────────────────────────────────────────────────────────────

print_header() {
  [ "$QUIET" -eq 1 ] && return 0
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    gum style --border normal --border-foreground 39 --padding "0 1" --margin "1 0" \
      "$(gum style --foreground 42 --bold 'ee installer')" \
      "$(gum style --foreground 245 'Durable, local-first, explainable memory for coding agents')"
  else
    echo ""
    echo -e "\033[1;32mee installer\033[0m"
    echo -e "\033[0;90mDurable, local-first, explainable memory for coding agents\033[0m"
    echo ""
  fi
}

# ───────────────────────────────────────────────────────────────────────────
# Main flow
# ───────────────────────────────────────────────────────────────────────────

print_header
print_agent_scan_notice
detect_agents
print_detected_agents

detect_platform
resolve_version
set_artifact_url

# Ensure the destination dir exists before write-perm check.
mkdir -p "$DEST" 2>/dev/null || true

preflight_checks

# Already-installed short-circuit (still configure shell completions).
if [ "$FROM_SOURCE" -eq 0 ] && [ "$FORCE_INSTALL" -eq 0 ] && [ -n "$VERSION" ] \
   && check_installed_version "$VERSION"; then
  ok "$PROJECT_LABEL $VERSION is already installed at $DEST/$BINARY"
  info "Use --force to reinstall"
  maybe_install_completions
  exit 0
fi

# Install the cleanup trap BEFORE acquiring resources. The previous order
# (acquire lock → mktemp → set trap) had a window where a failure between
# `mkdir "$LOCK_DIR"` and `trap cleanup EXIT` would leave the lock dir
# orphaned (e.g., `echo $$ > pid` fails under disk pressure, or `mktemp -d`
# fails on a removed external TMPDIR). Pre-initialize the state the cleanup
# closure consults so an early-fire trap is a safe no-op.
LOCKED=0
LOCK_DIR=""
TMP=""
cleanup() {
  [ -n "$TMP" ] && rm -rf "$TMP" 2>/dev/null || true
  if [ "$LOCKED" -eq 1 ] && [ -n "$LOCK_DIR" ]; then
    rm -rf "$LOCK_DIR" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# Cross-platform locking via mkdir (atomic on every POSIX FS).
LOCK_DIR="${LOCK_FILE}.d"
if mkdir "$LOCK_DIR" 2>/dev/null; then
  LOCKED=1
  # If writing the pid file fails (disk full, read-only FS), explicitly
  # release the lock dir so the next installer is not blocked by a stale
  # ownerless lock the stale-PID recovery cannot fix.
  if ! echo $$ > "$LOCK_DIR/pid" 2>/dev/null; then
    rm -rf "$LOCK_DIR" 2>/dev/null || true
    LOCKED=0
    err "Could not write pid file to $LOCK_DIR (disk full or read-only?)"
    exit 1
  fi
else
  if [ -f "$LOCK_DIR/pid" ]; then
    OLD_PID=$(cat "$LOCK_DIR/pid" 2>/dev/null || echo "")
    if [ -n "$OLD_PID" ] && ! kill -0 "$OLD_PID" 2>/dev/null; then
      rm -rf "$LOCK_DIR"
      if mkdir "$LOCK_DIR" 2>/dev/null; then
        LOCKED=1
        if ! echo $$ > "$LOCK_DIR/pid" 2>/dev/null; then
          rm -rf "$LOCK_DIR" 2>/dev/null || true
          LOCKED=0
          err "Could not write pid file to $LOCK_DIR (disk full or read-only?)"
          exit 1
        fi
      fi
    fi
  fi
  if [ "$LOCKED" -eq 0 ]; then
    err "Another installer is running (lock $LOCK_DIR). Re-run after it finishes."
    exit 1
  fi
fi

TMP=$(mktemp -d)

# ───────────────────────────────────────────────────────────────────────────
# Download or build
# ───────────────────────────────────────────────────────────────────────────

if [ "$FROM_SOURCE" -eq 0 ]; then
  info "Downloading $URL"
  if ! ee_curl "$URL" -o "$TMP/$TAR"; then
    warn "Artifact download failed; falling back to build-from-source"
    FROM_SOURCE=1
  fi
fi

if [ "$FROM_SOURCE" -eq 1 ]; then
  info "Building from source (requires git + Rust nightly)"
  ensure_rust
  if ! command -v git >/dev/null 2>&1; then
    err "git not found — required for --from-source"
    exit 1
  fi

  # First attempt: pinned to the requested tag/branch if any. If that fails
  # (tag doesn't exist, partial clone left a non-empty dest dir, etc.),
  # remove the dest and retry without --branch. Git refuses to clone into a
  # non-empty directory, so the cleanup is required for the fallback to
  # succeed — the previous version silently lost the fallback when the first
  # clone partially populated $TMP/src.
  branch_args=""
  [ -n "$VERSION" ] && branch_args="--branch $VERSION"
  # shellcheck disable=SC2086
  if ! git clone --depth 1 $branch_args "https://github.com/${OWNER}/${REPO}.git" "$TMP/src" 2>/dev/null; then
    rm -rf "$TMP/src"
    git clone --depth 1 "https://github.com/${OWNER}/${REPO}.git" "$TMP/src"
  fi

  (cd "$TMP/src" && run_with_spinner "Building $BINARY (release profile)" cargo build --release)

  # CARGO_TARGET_DIR may have redirected the build output (e.g., this project
  # documents a USB-NVMe redirect on the canonical Mac dev host). Probe the
  # default in-tree path first; if missing, ask cargo where it landed.
  SRC_BIN="$TMP/src/target/release/$BINARY"
  if [ ! -x "$SRC_BIN" ] && command -v cargo >/dev/null 2>&1; then
    discovered=$(cd "$TMP/src" && cargo metadata --no-deps --format-version 1 2>/dev/null \
                 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | head -1)
    [ -n "$discovered" ] && [ -x "$discovered/release/$BINARY" ] && SRC_BIN="$discovered/release/$BINARY"
  fi
  [ -x "$SRC_BIN" ] || { err "Build failed: $BINARY not produced at $SRC_BIN"; exit 1; }
  install -m 0755 "$SRC_BIN" "$DEST/$BINARY"
  ok "Installed to $DEST/$BINARY (built from source)"
else
  # Checksum + Sigstore verification.
  if [ "$NO_CHECKSUM" = "1" ]; then
    warn "Verification skipped (--no-verify / EE_SKIP_VERIFY=1)"
  else
    if [ -z "$CHECKSUM" ]; then
      [ -z "$CHECKSUM_URL" ] && CHECKSUM_URL="${URL}.sha256"
      info "Fetching checksum from ${CHECKSUM_URL}"
      if ! ee_curl "$CHECKSUM_URL" -o "$TMP/checksum.sha256"; then
        err "Checksum required and could not be fetched."
        err "Use --no-verify only if you have an out-of-band reason to trust this artifact."
        exit 1
      fi
      # `awk '{print $1}'` on a multi-line file would concatenate first
      # fields and produce a string that never matches the actual SHA256.
      # Pin to the first line so a multi-checksum file degrades gracefully.
      CHECKSUM=$(awk 'NR==1{print $1; exit}' "$TMP/checksum.sha256")
      if [ -z "$CHECKSUM" ]; then
        err "Empty checksum file"
        exit 1
      fi
    fi
    if ! verify_checksum "$TMP/$TAR" "$CHECKSUM"; then
      err "Installation aborted: checksum verification failed"
      exit 1
    fi
    if ! verify_sigstore_bundle "$TMP/$TAR" "$URL"; then
      err "Installation aborted: Sigstore verification failed"
      exit 1
    fi
  fi

  info "Extracting"
  mkdir -p "$TMP/extract"
  if command -v xz >/dev/null 2>&1; then
    xz -dc "$TMP/$TAR" | tar -xf - -C "$TMP/extract"
  elif command -v unxz >/dev/null 2>&1; then
    unxz -c "$TMP/$TAR" | tar -xf - -C "$TMP/extract"
  elif tar --help 2>&1 | grep -q -- '-J\|--xz'; then
    tar -xJf "$TMP/$TAR" -C "$TMP/extract"
  else
    err "xz decompression unavailable. Install xz-utils (apt/brew install xz)."
    exit 1
  fi

  BIN=$(find "$TMP/extract" -maxdepth 3 -type f -name "$BINARY" -perm -111 2>/dev/null | head -n 1)
  if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
    err "Binary '$BINARY' not found in archive after extraction"
    exit 1
  fi
  install -m 0755 "$BIN" "$DEST/$BINARY"
  ok "Installed to $DEST/$BINARY"
fi

# ───────────────────────────────────────────────────────────────────────────
# Post-install: PATH, completions, self-test
# ───────────────────────────────────────────────────────────────────────────

maybe_add_path
maybe_install_completions

if [ "$VERIFY" -eq 1 ]; then
  info "Running self-test"
  if "$DEST/$BINARY" --version >/dev/null 2>&1; then
    "$DEST/$BINARY" --version || true
  else
    warn "$BINARY --version returned non-zero"
  fi
  if "$DEST/$BINARY" doctor --json >/dev/null 2>&1; then
    ok "ee doctor: pass"
  else
    warn "ee doctor reported issues — run 'ee doctor --json | jq .' to inspect"
  fi
fi

# ───────────────────────────────────────────────────────────────────────────
# Agent integration instructions
# ───────────────────────────────────────────────────────────────────────────

print_integration_snippet() {
  local title="$1" body="$2"
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    gum style --foreground 39 --bold "  → $title"
    while IFS= read -r line; do
      gum style --foreground 245 "      $line"
    done <<<"$body"
  else
    echo -e "  \033[1;34m→ ${title}\033[0m"
    while IFS= read -r line; do
      echo -e "      \033[0;90m${line}\033[0m"
    done <<<"$body"
  fi
}

show_agent_integration() {
  [ "$NO_CONFIGURE" -eq 1 ] && return 0
  [ "$QUIET" -eq 1 ] && return 0
  [[ ${#DETECTED_AGENTS[@]} -eq 0 ]] && return 0

  echo ""
  if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
    gum style --foreground 212 --bold "Agent integration"
    gum style --foreground 245 "ee is harness-agnostic — wire it into your agents at your own pace."
  else
    echo -e "\033[1;35mAgent integration\033[0m"
    echo -e "\033[0;90mee is harness-agnostic — wire it into your agents at your own pace.\033[0m"
  fi
  echo ""

  if is_agent_detected "claude-code"; then
    print_integration_snippet "Claude Code (~/.claude/AGENTS.md or CLAUDE.md)" "\
Before risky shell commands:
  ee preflight check --cmd \"<shell command>\" --workspace . --json

Before substantial work:
  ee context \"<task>\" --workspace . --max-tokens 4000 --format markdown

Or wire as a PreToolUse hook (writes a shell snippet to stdout):
  $DEST/$BINARY hook preflight-shell --shell bash"
    echo ""
  fi

  if is_agent_detected "codex-cli"; then
    print_integration_snippet "Codex CLI (~/.codex/AGENTS.md)" "\
Before substantial work:
  ee context \"<task>\" --workspace . --json

Optional risk guard (call from codex shell hooks):
  ee preflight check --cmd \"<command>\" --workspace . --json"
    echo ""
  fi

  if is_agent_detected "gemini-cli"; then
    print_integration_snippet "Gemini CLI (~/.gemini/GEMINI.md or settings.json)" "\
For BeforeTool integration, see the ee docs:
  $DEST/$BINARY hook preflight-shell --shell bash
For context packs:
  ee context \"<task>\" --workspace . --json"
    echo ""
  fi

  if is_agent_detected "cursor-ide"; then
    print_integration_snippet "Cursor IDE (~/.cursor/hooks.json)" "\
Cursor's beforeShellExecution hook can call:
  ee preflight check --cmd \"\$COMMAND\" --workspace . --json
The exact wrapper script depends on Cursor's hook payload shape; see
docs/agent-ux/auto_enrollment_onboarding.md for current guidance."
    echo ""
  fi

  if is_agent_detected "aider" || is_agent_detected "continue" || is_agent_detected "github-copilot-cli"; then
    print_integration_snippet "Aider / Continue / Copilot CLI" "\
These harnesses don't have a documented PreToolUse hook surface for ee yet.
You can still call ee directly from your prompt setup:
  ee context \"<task>\" --workspace . --json"
    echo ""
  fi
}

# ───────────────────────────────────────────────────────────────────────────
# Final summary
# ───────────────────────────────────────────────────────────────────────────

show_agent_integration

[ "$QUIET" -eq 1 ] && exit 0

echo ""
if [ "$HAS_GUM" -eq 1 ] && [ "$NO_GUM" -eq 0 ]; then
  {
    gum style --foreground 42 --bold "ee is installed!"
    echo ""
    gum style --foreground 245 "Binary:        $DEST/$BINARY"
    [ -n "$VERSION" ] && gum style --foreground 245 "Version:       $VERSION"
    gum style --foreground 245 "Target:        ${TARGET:-source-build}"
    echo ""
    gum style --foreground 245 "Get started:"
    gum style --foreground 39  "  ee init --workspace ."
    gum style --foreground 39  "  ee context \"<task>\" --workspace . --max-tokens 4000"
    gum style --foreground 39  "  ee --help"
    echo ""
    gum style --foreground 245 --italic "Inspect health:  ee doctor --json"
    gum style --foreground 245 --italic "Uninstall:       rm $DEST/$BINARY (config in ~/.ee/ and ~/.local/share/ee/ persists)"
  } | gum style --border normal --border-foreground 42 --padding "1 2"
else
  echo -e "\033[1;32mee is installed!\033[0m"
  echo ""
  echo -e "  Binary:        \033[0;36m$DEST/$BINARY\033[0m"
  [ -n "$VERSION" ] && echo -e "  Version:       \033[0;36m$VERSION\033[0m"
  echo -e "  Target:        \033[0;36m${TARGET:-source-build}\033[0m"
  echo ""
  echo -e "  Get started:"
  echo -e "    \033[0;34mee init --workspace .\033[0m"
  echo -e "    \033[0;34mee context \"<task>\" --workspace . --max-tokens 4000\033[0m"
  echo -e "    \033[0;34mee --help\033[0m"
  echo ""
  echo -e "  \033[0;90mInspect health:  ee doctor --json\033[0m"
  echo -e "  \033[0;90mUninstall:       rm $DEST/$BINARY  (config in ~/.ee/ and ~/.local/share/ee/ persists)\033[0m"
fi
