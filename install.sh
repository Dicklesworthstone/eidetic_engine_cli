#!/usr/bin/env bash
#
# ee (Eidetic Engine CLI) installer
# Durable, local-first, explainable memory for coding agents.
#
# One-liner install:
#   curl -fsSL "https://raw.githubusercontent.com/Dicklesworthstone/eidetic_engine_cli/main/install.sh" | bash
#
# Do not add a cache-busting query string (e.g. ?$(date +%s)). A unique URL
# per request defeats the CDN and forces an origin fetch every time, which is
# the pattern GitHub's anti-scraping limiter penalises -- a real client
# install failed with 429 Too Many Requests on raw.githubusercontent.com for
# exactly this reason. The plain URL is CDN-cacheable and re-reads pick up a
# new main within the cache TTL anyway.
#
# Pinned version:
#   curl -fsSL https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.sh | EE_VERSION=v0.1.0 bash
#
# Options:
#   --version vX.Y.Z   Install specific version (default: latest)
#   --dest DIR         Install to DIR (default: ~/.local/bin)
#   --system           Install to /usr/local/bin (requires sudo)
#   --easy-mode        Persist PATH in the active zsh/bash startup file
#   --verify           Run `ee --version` and `ee doctor --json` self-test
#   --from-source      Build from source instead of downloading binary
#   --quiet, -q        Suppress non-error output
#   --offline          Skip network preflight checks (use with --artifact-url)
#   --no-gum           Disable gum formatting even if available
#   --no-configure     Skip agent integration instructions (still installs binary)
#   --no-verify        Skip checksum + Sigstore verification (NOT recommended)
#   --require-provenance
#                       Require SLSA provenance JSON + Sigstore verification
#   --force            Force reinstall even when same version is already present
#
# Environment variables (legacy names honored for back-compat):
#   EE_VERSION         specific version to install (== --version)
#   EE_INSTALL_DIR     installation directory (== --dest)
#   EE_SKIP_VERIFY     set to 1 to skip verification (== --no-verify)
#   EE_REQUIRE_PROVENANCE
#                      set to 1 to require provenance verification
#   EE_INSTALL_REQUIRE_KEYLESS
#                      set to 1 to refuse pinned-key fallback verification
#   EE_INSTALL_SEMANTIC_SMOKE
#                      warn|require: prove Model2Vec + native reranker first use
#   EE_INSTALL_RERANK_MODEL_URL
#                      alternate reranker archive URL for the first-use smoke;
#                      bytes must still match the manifest embedded in ee
#   HTTPS_PROXY / HTTP_PROXY   honored for every network call
#
set -euo pipefail
umask 022
shopt -s lastpipe 2>/dev/null || true

# Apple still ships Bash 3.2, which treats an empty "${array[@]}" expansion
# as an unbound variable under `set -u`. Every expansion of a possibly empty
# array in this installer must use "${array[@]+"${array[@]}"}" instead.

# ───────────────────────────────────────────────────────────────────────────
# Defaults & CLI state
# ───────────────────────────────────────────────────────────────────────────

# Last resort when the GitHub release API is unusable (degraded, or an
# unauthenticated rate limit -- measured 2026-08-17: the releases endpoint
# returned an EMPTY ARRAY to an unauthenticated Windows host while an
# authenticated query from another machine listed six releases). Pinned to
# the newest release verified to publish a complete asset matrix, matching
# install.ps1's $Script:LastKnownGoodTag so the two installers cannot diverge
# in what they consider known-good.
EE_LAST_KNOWN_GOOD_TAG="v0.13.0"

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
#
# Parallel arrays of (identity_regexp, oidc_issuer) trust pairs. Verification
# accepts an artifact if ANY pair matches. The first entry is the canonical
# CI-built identity (release.yml on GitHub Actions); subsequent entries are
# explicitly enumerated manual-release fallbacks for releases cut while
# release.yml is throttled, broken, or otherwise unavailable. Manual-release
# anchors MUST be pinned to a specific maintainer's verified identity + the
# exact OIDC issuer that maintainer used at signing time; do not relax these
# to wildcards.
CERT_IDENTITY_REGEXPS=(
  "^https://github\.com/${OWNER}/${REPO}/\.github/workflows/release\.yml@refs/tags/v[0-9].*$"
  '^jeff141421@gmail\.com$'
)
CERT_OIDC_ISSUERS=(
  "https://token.actions.githubusercontent.com"
  "https://github.com/login/oauth"
)
# First entry is also exposed under the legacy single-value names so the
# informational logs and any downstream consumer that still reads them
# continue to surface the canonical CI identity.
CERT_IDENTITY_REGEXP="${CERT_IDENTITY_REGEXPS[0]}"
CERT_OIDC_ISSUER="${CERT_OIDC_ISSUERS[0]}"

# Long-lived maintainer-controlled signing key for fully-automated manual
# releases. Embedded here so install.sh is self-contained — no extra
# network round-trip to fetch the public key, and rotation requires
# re-rolling install.sh (which lives in the same Release as the signed
# binaries, so a compromised release cannot silently pin a hostile key).
#
# Key generation (one-time, on a maintainer host):
#   COSIGN_PASSWORD="" cosign generate-key-pair --output-key-prefix=cosign-ee
#   cp cosign-ee.key ~/.config/ee-signing/cosign-ee.key   # private, 0600
#   # cosign-ee.pub contents → embedded below + committed to signing/cosign.pub
#
# Signing (automated, per release):
#   COSIGN_PASSWORD="" cosign sign-blob --yes \
#     --tlog-upload=true \
#     --key ~/.config/ee-signing/cosign-ee.key \
#     --bundle <file>.sigstore.json <file>
EE_RELEASE_SIGNING_PUBLIC_KEY='-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE35liCkNgYT2g39ERvnWWRuu4zFVJ
h3VXjvm63PcMNKFcvqq39g3UIGwQMLdNPwkiPHM4lqE2vrQOoAHcRIXf4Q==
-----END PUBLIC KEY-----'

# Try every supported verification path against $bundle + $payload. Returns
# 0 on the first success, 1 if no path matched. Order matters: keyless
# identity-bound certificates first (CI builds and historical device-flow
# maintainer releases), then the pinned long-lived key as a visible fallback
# for manual maintainer-cut releases. Stderr is suppressed per attempt so a
# release signed by one path does not surface the other paths' "no matching
# certificate" message as a user-facing error. Caller is responsible for the
# final user-facing failure message.
verify_blob_against_anchors() {
  local bundle="$1" payload="$2"

  # Path 1..N: keyless identity-bound certs (CI builds + device-flow).
  local i
  for i in "${!CERT_IDENTITY_REGEXPS[@]}"; do
    if cosign verify-blob \
          --bundle "$bundle" \
          --insecure-ignore-tlog=false \
          --certificate-identity-regexp "${CERT_IDENTITY_REGEXPS[$i]}" \
          --certificate-oidc-issuer "${CERT_OIDC_ISSUERS[$i]}" \
          "$payload" >/dev/null 2>&1; then
      return 0
    fi
  done

  if [ "$REQUIRE_KEYLESS" = "1" ]; then
    return 1
  fi

  # Fallback path: pinned long-lived key for manual maintainer-cut releases.
  local pubkey_file
  pubkey_file="$TMP/ee-release-signing-key.pub"
  printf '%s\n' "$EE_RELEASE_SIGNING_PUBLIC_KEY" > "$pubkey_file"
  if cosign verify-blob \
        --bundle "$bundle" \
        --insecure-ignore-tlog=false \
        --key "$pubkey_file" \
        "$payload" >/dev/null 2>&1; then
    info "Sigstore verified via pinned-key fallback for $(basename "$payload")"
    return 0
  fi

  return 1
}

EASY=0
QUIET=0
VERIFY=0
FROM_SOURCE=0
NO_GUM=0
NO_CONFIGURE=0
NO_CHECKSUM="${EE_SKIP_VERIFY:-0}"
REQUIRE_PROVENANCE="${EE_REQUIRE_PROVENANCE:-0}"
REQUIRE_KEYLESS="${EE_INSTALL_REQUIRE_KEYLESS:-0}"
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

  for line in "${lines[@]+"${lines[@]}"}"; do
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
  for line in "${lines[@]+"${lines[@]}"}"; do
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

# Backoff schedule for retried requests (seconds before attempt 1, 2, 3).
# Mirrors ACFS_CURL_RETRY_DELAYS in agentic_coding_flywheel_setup's
# scripts/lib/security.sh (7a59cb29) so the two installers retry alike.
EE_CURL_RETRY_DELAYS=(0 5 15)

# Decide whether an HTTP status is worth retrying.
#
# curl collapses EVERY HTTP status >= 400 into exit code 22, so the exit
# code alone cannot tell a rate limit from a genuine 404. Retrying on bare
# 22 would hammer a missing URL forever; refusing to retry 22 outright
# treats a rate limit as PERMANENT, which is backwards -- rate limiting is
# the most retryable failure there is. A real client install died this way:
# raw.githubusercontent.com answered 429 and the install never started.
#
# Retry: 429 (rate limited), 503 (unavailable), 502/504 (transient gateway).
# Fatal: 404 and 403 -- retrying cannot change those answers.
ee_is_retryable_http_status() {
  case "${1:-0}" in
    429|503|502|504) return 0 ;;
    *) return 1 ;;
  esac
}

# Seconds to wait per the server's Retry-After header, echoed on stdout.
# Empty when absent or unusable. Supports the delta-seconds form only; an
# HTTP-date form is ignored deliberately rather than mis-parsed into a wrong
# delay. Clamped so a hostile or absurd header cannot stall an install
# indefinitely.
ee_retry_after_seconds() {
  local headers_file="${1:-}"
  [ -s "$headers_file" ] || return 0
  local value=""
  value=$(grep -i '^retry-after:' "$headers_file" 2>/dev/null | tail -1 \
    | sed 's/^[Rr]etry-[Aa]fter:[[:space:]]*//; s/[[:space:]]*$//') || value=""
  case "$value" in
    ''|*[!0-9]*) return 0 ;;
  esac
  [ "$value" -gt 300 ] 2>/dev/null && value=300
  printf '%s' "$value"
}

# Curl wrapper that honors proxy and standard timeouts, and retries transient
# failures -- both connection-level (DNS/connect/timeout/SSL/empty-reply) and
# HTTP-status-level (429/503/502/504, classified from the actual response
# status line since curl's own exit code cannot distinguish those from a
# fatal 404/403). Treat as `curl -fsSL` with proxy + retry pre-wired.
#
# Defaults --connect-timeout / --max-time so a hung proxy or stalled mirror
# can't make the installer wait indefinitely (the previous shape relied
# only on default kernel TCP timeouts, which can hold the installer in
# read() forever once a connection is established). Callers can override
# either ceiling by passing the flag again — curl honors the LAST
# occurrence of each option, so the preflight check at line 626
# (--connect-timeout 3 --max-time 5) and any other tighter caller still
# tightens, never loosens. The 600s --max-time is generous enough for the
# largest expected tarball download on a slow link without letting a stuck
# server pin the installer past a normal operator's patience window.
ee_curl() {
  # Stock macOS ships Bash 3.2, where expanding an empty array under
  # `set -u` raises "unbound variable". The `+word` form expands to zero
  # arguments when PROXY_ARGS is empty and preserves both proxy arguments
  # when it is populated.
  local attempt=0 status=0 hdr_file="" http_status="" server_delay=""
  local retryable=0

  while :; do
    if [ "$attempt" -gt 0 ]; then
      sleep "${EE_CURL_RETRY_DELAYS[$((attempt - 1))]:-15}"
    fi

    hdr_file="$(mktemp "${TMPDIR:-/tmp}/ee-curl-hdr.XXXXXX" 2>/dev/null || true)"
    if [ -n "$hdr_file" ]; then
      curl -fsSL \
        --connect-timeout 15 \
        --max-time 600 \
        "${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"}" -D "$hdr_file" "$@"
      status=$?
    else
      curl -fsSL \
        --connect-timeout 15 \
        --max-time 600 \
        "${PROXY_ARGS[@]+"${PROXY_ARGS[@]}"}" "$@"
      status=$?
    fi

    if [ "$status" -eq 0 ]; then
      [ -n "$hdr_file" ] && rm -f "$hdr_file" 2>/dev/null
      return 0
    fi

    retryable=0
    case "$status" in
      6|7|28|35|52|56) retryable=1 ;; # DNS/connect/timeout/SSL/empty-reply/recv-error
      22)
        if [ -n "$hdr_file" ] && [ -s "$hdr_file" ]; then
          http_status=$(grep -oE '^HTTP/[0-9.]+ [0-9]{3}' "$hdr_file" 2>/dev/null \
            | tail -1 | awk '{print $2}') || http_status=""
          if [ -n "$http_status" ] && ee_is_retryable_http_status "$http_status"; then
            retryable=1
            server_delay=$(ee_retry_after_seconds "$hdr_file") || server_delay=""
            # info() writes to stdout (unlike warn/err), but ee_curl's own
            # stdout is a live data channel for most callers (JSON bodies,
            # tags, piped installers via `$(ee_curl ...)`), so these must
            # go to stderr explicitly rather than through plain info -- an
            # unredirected info() call here would splice status text into
            # the next successful attempt's captured response body.
            if [ -n "$server_delay" ]; then
              info "HTTP $http_status; honouring Retry-After: ${server_delay}s" >&2
              sleep "$server_delay"
            else
              info "HTTP $http_status; retrying with backoff" >&2
            fi
          elif [ -n "$http_status" ]; then
            # Informational, not a warning: this is one HTTP transaction's
            # status, not a verdict on the install. ee_curl has no idea
            # whether the caller has a fallback (e.g. a compatible release
            # target) that makes this attempt's failure routine rather than
            # fatal -- only the caller knows that, and a caller with no
            # recovery left already surfaces its own warn/err on top of
            # this. Printing this at warn level unconditionally is what
            # made a successful musl->gnu fallback install read as broken:
            # three yellow warnings in a row for a release-matrix gap that
            # the installer was always going to recover from automatically.
            info "HTTP $http_status is not retryable" >&2
          fi
        fi
        ;;
    esac

    [ -n "$hdr_file" ] && rm -f "$hdr_file" 2>/dev/null
    attempt=$((attempt + 1))

    if [ "$retryable" -eq 0 ] || [ "$attempt" -gt "${#EE_CURL_RETRY_DELAYS[@]}" ]; then
      return "$status"
    fi
  done
}

# Reduce a GitHub `/releases` JSON response (stdin) to one line per release
# record, newest-first as GitHub orders them:
#
#   <tag_name> <draft> <prerelease> <has_asset>
#
# where <has_asset> is 1 when an asset named exactly $1 is attached to that
# release and 0 otherwise (always 0 when $1 is empty). Records with no
# tag_name are dropped. Pure awk, single pass, no per-record subprocesses,
# so a moderately sized (sub-MB) response costs one scan regardless of
# whether GitHub pretty-prints it (it does) or sends it compact (GH#31).
#
# Splitting on the literal `"tag_name"` key is safe: assets and uploader
# objects carry no such key, and any occurrence inside a release body is
# JSON-escaped as \"tag_name\", which does not match the unescaped key.
#
# The newlines are stripped up front so awk sees one record and never
# rebuilds a growing buffer per line (quadratic on BSD awk: ~3s for the
# 13k-line pretty-printed response vs ~0.2s joined).
ee_release_records() {
  LC_ALL=C tr -d '\n\r' | LC_ALL=C awk -v asset="$1" '
    function field(s, re,    m) {
      if (match(s, re) == 0) return ""
      m = substr(s, RSTART, RLENGTH)
      sub(/^[^:]*:[ \t]*"/, "", m)
      sub(/"$/, "", m)
      return m
    }
    function word(s, re,    m) {
      if (match(s, re) == 0) return ""
      m = substr(s, RSTART, RLENGTH)
      sub(/^[^:]*:[ \t]*/, "", m)
      return m
    }
    { buf = buf $0 }
    END {
      key = "\"tag_name\""
      start = index(buf, key)
      if (start == 0) exit 0
      buf = substr(buf, start)
      while (buf != "") {
        nxt = index(substr(buf, 2), key)
        if (nxt == 0) {
          rec = buf
          buf = ""
        } else {
          rec = substr(buf, 1, nxt)
          buf = substr(buf, nxt + 1)
        }
        tag = field(rec, "\"tag_name\"[ \t]*:[ \t]*\"[^\"]*\"")
        if (tag == "") continue
        draft = word(rec, "\"draft\"[ \t]*:[ \t]*[a-z]+")
        pre = word(rec, "\"prerelease\"[ \t]*:[ \t]*[a-z]+")
        has = 0
        if (asset != "" && (index(rec, "\"name\": \"" asset "\"") > 0 || index(rec, "\"name\":\"" asset "\"") > 0)) has = 1
        print tag, (draft == "" ? "false" : draft), (pre == "" ? "false" : pre), has
      }
    }
  '
}

# ───────────────────────────────────────────────────────────────────────────
# Usage
# ───────────────────────────────────────────────────────────────────────────

usage() {
  cat <<EOFU
Usage: install.sh [--version vX.Y.Z] [--dest DIR] [--system] [--easy-mode] [--verify] \\
                  [--artifact-url URL] [--checksum HEX] [--checksum-url URL] \\
                  [--from-source] [--quiet] [--offline] [--no-gum] [--no-configure] \\
                  [--no-verify] [--require-provenance] [--force]

Options:
  --version vX.Y.Z   Install specific version (default: latest GitHub release)
  --dest DIR         Install to DIR (default: ~/.local/bin)
  --system           Install to /usr/local/bin (requires write permission)
  --easy-mode        Persist PATH in zsh/bash startup files, creating the active one if absent
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
  --require-provenance
                      Require SLSA provenance JSON + Sigstore verification
  --force            Reinstall even when the same version is already present

Environment variables:
  EE_VERSION         == --version
  EE_INSTALL_DIR     == --dest
  EE_SKIP_VERIFY=1   == --no-verify
  EE_REQUIRE_PROVENANCE=1
                     == --require-provenance
  EE_INSTALL_REQUIRE_KEYLESS=1
                     refuse pinned-key fallback; require a keyless Sigstore identity match
  EE_INSTALL_SEMANTIC_SMOKE=warn|require
                     run the Model2Vec + native-reranker first-use smoke; require fails closed
  EE_INSTALL_RERANK_MODEL_URL=URL
                     alternate reranker archive URL for that smoke (embedded hashes still apply)
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
    --system) DEST="/usr/local/bin"; shift;;
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
    --require-provenance) REQUIRE_PROVENANCE=1; shift;;
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

case "$REQUIRE_PROVENANCE" in
  0|1) ;;
  *) err "EE_REQUIRE_PROVENANCE must be 0 or 1"; exit 2;;
esac

case "$REQUIRE_KEYLESS" in
  0|1) ;;
  *) err "EE_INSTALL_REQUIRE_KEYLESS must be 0 or 1"; exit 2;;
esac

if [ "$REQUIRE_PROVENANCE" = "1" ] && [ "$NO_CHECKSUM" = "1" ]; then
  err "--require-provenance cannot be combined with --no-verify / EE_SKIP_VERIFY=1"
  exit 2
fi

if [ "$REQUIRE_KEYLESS" = "1" ] && [ "$NO_CHECKSUM" = "1" ]; then
  err "EE_INSTALL_REQUIRE_KEYLESS=1 cannot be combined with --no-verify / EE_SKIP_VERIFY=1"
  exit 2
fi

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
  for agent in "${DETECTED_AGENTS[@]+"${DETECTED_AGENTS[@]}"}"; do
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
  for agent in "${DETECTED_AGENTS[@]+"${DETECTED_AGENTS[@]}"}"; do
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
FALLBACK_TARGET=""

detect_platform() {
  OS=$(uname -s | tr '[:upper:]' '[:lower:]')
  ARCH=$(uname -m)
  TARGET=""
  FALLBACK_TARGET=""
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
      # Prefer the portable musl artifact, but retain the glibc build as a
      # compatible release fallback. Some historical releases (including
      # v0.12.0) shipped only the GNU archive; those installs must not turn
      # into an unexpected local Rust build.
      TARGET="x86_64-unknown-linux-musl"
      FALLBACK_TARGET="x86_64-unknown-linux-gnu"
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

  local tarball_name=""
  if [ -n "$TARGET" ]; then
    tarball_name="ee-${TARGET}.tar.xz"
    info "Resolving latest version carrying ${tarball_name}…"
  else
    info "Resolving latest version…"
  fi

  local latest_api="https://api.github.com/repos/${OWNER}/${REPO}/releases/latest"
  local latest_tag=""
  latest_tag=$(ee_curl -H "Accept: application/vnd.github.v3+json" "$latest_api" 2>/dev/null \
        | grep '"tag_name":' | head -1 \
        | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/') || latest_tag=""

  local tag=""

  # Enumerate recent releases newest-first and pick the first stable one that
  # actually ships this platform's tarball, skipping drafts and prereleases.
  # /releases/latest excludes prereleases by GitHub API contract, but says
  # nothing about which assets that release carries -- v0.13.1 published only
  # aarch64-apple-darwin and x86_64-unknown-linux-gnu, so /releases/latest
  # alone would resolve to a tag whose download 404s for every other
  # platform. Mirrors install.ps1's Get-LatestVersion.
  if [ -n "$tarball_name" ]; then
    local releases_api="https://api.github.com/repos/${OWNER}/${REPO}/releases?per_page=20"
    local releases_json=""
    releases_json=$(ee_curl -H "Accept: application/vnd.github.v3+json" "$releases_api" 2>/dev/null) \
      || releases_json=""

    # Three states, not two. (1) Enumeration genuinely succeeded and a
    # scanned release ships this asset -- tag gets set below. (2)
    # Enumeration genuinely succeeded and scanned real release records but
    # NONE of them ship it -- a real finding, worth the confident warning.
    # (3) Enumeration produced nothing usable: ee_curl failed outright, OR
    # it returned 200 with a body that parses to zero release records (a
    # bare "[]", or a truncated/degraded response). (3) is UNKNOWN, not
    # "confirmed absent", and must never render as (2)'s warning -- an
    # empty-but-200 response is not evidence the asset is missing. Measured
    # 2026-08-17: an unauthenticated /releases?per_page=20 call returned an
    # empty array to one host while an authenticated call from another
    # machine listed six releases for the same repo at the same time.
    # records_seen distinguishes (3) from (2): it counts real release
    # records parsed out of the response (regardless of whether any of them
    # matched this platform's tarball), so "[]" and "curl failed outright"
    # both leave it at 0 and converge on the same honest, low-confidence
    # message.
    local records_seen=0
    if [ -n "$releases_json" ]; then
      local records="" rel_tag="" rel_draft="" rel_prerelease="" rel_has_asset=""
      # One awk pass turns the whole response into at most one short line
      # per release (see ee_release_records). The previous shape -- split
      # the raw JSON on "tag_name" with sed and run a grep|head|sed
      # pipeline per chunk inside a while/read loop -- assumed a compact
      # single-line body, but api.github.com pretty-prints: every line of a
      # ~800 KB response became its own "record", each one forked several
      # processes, and the installer sat for minutes in what looked like a
      # hang (GH#31). Doing the parse in one process bounds this to the
      # cost of scanning the response once.
      records=$(printf '%s\n' "$releases_json" | ee_release_records "$tarball_name") || records=""
      # `|| [ -n "$rel_tag" ]` keeps an unterminated final line in play:
      # plain `read` returns nonzero for it and would silently drop the
      # last record.
      while read -r rel_tag rel_draft rel_prerelease rel_has_asset || [ -n "$rel_tag" ]; do
        [ -n "$rel_tag" ] || continue
        records_seen=$((records_seen + 1))
        [ "$rel_draft" = "true" ] && continue
        [ "$rel_prerelease" = "true" ] && continue
        [ "$rel_has_asset" = "1" ] || continue
        if [ -n "$latest_tag" ] && [ "$rel_tag" != "$latest_tag" ]; then
          warn "Latest release $latest_tag does not include ${tarball_name}."
          warn "Falling back to $rel_tag, the newest stable release that does."
          warn "This is an upstream release-matrix gap, not a problem with your machine."
        fi
        tag="$rel_tag"
        break
      done <<EOF
$records
EOF
    fi

    if [ -z "$tag" ]; then
      if [ "$records_seen" -eq 0 ]; then
        # State (3): unknown, not confirmed-absent. Same message whether
        # ee_curl failed outright or returned an empty/unparseable body.
        warn "Could not enumerate releases (empty or unusable response); falling through to the latest release."
      elif [ -n "$FALLBACK_TARGET" ]; then
        # A compatible fallback target exists (e.g. musl -> gnu on
        # x86_64 Linux) and download_release_artifact will try it
        # automatically if the preferred target's download fails. This
        # is an expected, self-healing path, not a warning: state
        # plainly what is happening in one sentence instead of the
        # generic alarm below.
        info "${tarball_name} is not published for the last 20 releases; using the compatible ${FALLBACK_TARGET} build if needed."
      else
        warn "No stable release in the last 20 ships ${tarball_name}."
      fi
    fi
  fi

  # Enumeration failed or found nothing: fall back to whatever /latest said
  # rather than refusing outright, so a transient API problem cannot block a
  # user who may already have a working asset there.
  if [ -z "$tag" ] && [ -n "$latest_tag" ]; then
    if [ -z "$FALLBACK_TARGET" ]; then
      # When a compatible fallback target exists, the asset-presence check
      # just above already said the one sentence that explains this --
      # repeating it here in a second line would be redundant chatter, not
      # extra clarity.
      warn "Proceeding with $latest_tag; if its ${tarball_name:-tarball} is absent the download will fail."
    fi
    tag="$latest_tag"
  fi

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
    # Last resort: the GitHub release API is unusable (degraded, or an
    # unauthenticated rate limit -- measured 2026-08-17: the releases
    # endpoint returned an EMPTY ARRAY to an unauthenticated Windows host
    # while an authenticated query from another machine listed six
    # releases). An installer that can only work when api.github.com is
    # healthy is not robust enough to bootstrap a fresh machine, so fall
    # through to a pinned tag known to carry a full asset matrix rather
    # than refusing outright.
    warn "GitHub release API returned nothing usable."
    warn "Falling back to pinned last-known-good tag ${EE_LAST_KNOWN_GOOD_TAG}."
    warn "If that is older than you expect, re-run with --version vX.Y.Z once GitHub recovers."
    tag="$EE_LAST_KNOWN_GOOD_TAG"
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

select_compatible_fallback_target() {
  [ -n "$FALLBACK_TARGET" ] || return 1

  # A caller-provided URL or checksum is bound to one exact artifact. Never
  # silently retarget those trust inputs to a different release archive.
  if [ -n "$ARTIFACT_URL" ] || [ -n "$CHECKSUM" ] || [ -n "$CHECKSUM_URL" ]; then
    return 1
  fi

  local failed_target="$TARGET"
  TARGET="$FALLBACK_TARGET"
  FALLBACK_TARGET=""
  set_artifact_url
  # This firing IS the automatic recovery working as designed -- state
  # plainly what happened, at info level, not warn: a client watching the
  # preceding informational lines about the preferred target should read
  # this as "and here is the compatible build being used", the one
  # intentional sentence that explains the whole sequence, not a fourth
  # alarm stacked on three others for an install that is about to succeed.
  info "${failed_target} not published for this release; using the compatible ${TARGET} build."
  return 0
}

download_release_artifact() {
  info "Downloading $URL"
  if ee_curl "$URL" -o "$TMP/$TAR"; then
    return 0
  fi

  if select_compatible_fallback_target; then
    info "Downloading $URL"
    if ee_curl "$URL" -o "$TMP/$TAR"; then
      return 0
    fi
  fi

  return 1
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
  # Probe a single byte rather than downloading the full release archive once
  # here and then again during installation. GitHub Release assets honor range
  # requests; mirrors that ignore the range still remain bounded by max-time.
  if ! ee_curl --connect-timeout 3 --max-time 5 --range 0-0 -o /dev/null "$URL" 2>/dev/null; then
    if [ -n "$FALLBACK_TARGET" ]; then
      # Same reasoning as resolve_version's asset-presence check: a
      # compatible fallback target exists and will be tried automatically,
      # so a failed probe against the PREFERRED target's URL here is
      # expected, not a warning.
      info "Preferred ${TARGET} artifact not reachable at $URL; will try the compatible ${FALLBACK_TARGET} build if needed."
    else
      warn "Network check failed for $URL — continuing; download may fail"
    fi
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
  local version_output=""
  local installed
  if ! version_output=$("$DEST/$BINARY" --version 2>/dev/null); then
    return 1
  fi
  # BSD sed (macOS) treats `\+` as literal `+`, not the GNU "one or more"
  # quantifier — so the prior regex silently failed to match on macOS,
  # making check_installed_version always return 1 (broken short-circuit,
  # benign re-install). Use portable POSIX BRE: `[[:space:]][[:space:]]*`.
  installed=$(printf '%s\n' "$version_output" | head -1 \
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
  local path_active=0
  case ":$PATH:" in
    *:"$DEST":*) path_active=1 ;;
  esac

  if [ "$EASY" -ne 1 ]; then
    if [ "$path_active" -eq 0 ]; then
      warn "Add $DEST to PATH to use $BINARY (or re-run with --easy-mode to auto-update)"
    fi
    return 0
  fi

  # `--easy-mode` promises persistent integration, so do not return merely
  # because this process inherited DEST on PATH. A transient PATH entry must
  # not hide a missing startup-file entry on a matching-version repair run.
  local preferred_rc=""
  case "${SHELL:-}" in
    */zsh) preferred_rc="$HOME/.zshrc" ;;
    */bash) preferred_rc="$HOME/.bashrc" ;;
  esac

  # Brand-new macOS/Linux accounts may not have an rc file yet. Create only
  # the active supported shell's file, refuse dangling symlinks, and use a
  # private creation mode. Existing writable zsh/bash files are still kept in
  # sync for users who switch between the two shells.
  if [ -n "$preferred_rc" ] && [ ! -e "$preferred_rc" ] && [ ! -L "$preferred_rc" ] \
     && [ -d "$HOME" ] && [ -w "$HOME" ]; then
    (umask 077; : > "$preferred_rc") 2>/dev/null || true
  fi

  local appended=0
  local configured=0
  local path_line="export PATH=\"$DEST:\$PATH\""
  local rc
  for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
    if [ -f "$rc" ] && [ -w "$rc" ]; then
      if grep -F "$DEST" "$rc" >/dev/null 2>&1; then
        configured=1
      elif printf '%s\n' "$path_line" >> "$rc"; then
        appended=1
        configured=1
      else
        warn "Could not update PATH in $rc"
      fi
    fi
  done

  if [ "$appended" -eq 1 ]; then
    warn "PATH updated in shell startup files — restart your shell to use $BINARY"
  elif [ "$configured" -eq 1 ]; then
    info "PATH already configured in shell startup files — restart your shell to use $BINARY"
  elif [ "$path_active" -eq 1 ]; then
    warn "$DEST is active in this shell, but no writable zsh/bash startup file was available to persist it"
  else
    warn "Add $DEST to PATH to use $BINARY (no writable zsh/bash startup file was available)"
  fi
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

# `--verify` validates the executable itself strictly while keeping doctor
# posture advisory. A fresh database or unavailable optional capability can
# make doctor non-zero without meaning the installed binary is unusable.
run_install_self_test() {
  info "Running self-test"

  local version_output=""
  local version_status=0
  if version_output=$("$DEST/$BINARY" --version 2>&1); then
    version_status=0
  else
    version_status=$?
    err "$BINARY --version failed with exit code $version_status"
    [ -n "$version_output" ] && printf '%s\n' "$version_output" >&2
    return 1
  fi
  if [ -z "$version_output" ]; then
    err "$BINARY --version returned no output"
    return 1
  fi
  printf '%s\n' "$version_output"

  if "$DEST/$BINARY" doctor --json >/dev/null 2>&1; then
    ok "ee doctor: pass"
  else
    warn "ee doctor reported issues — run 'ee doctor --json | jq .' to inspect"
  fi
}

# ───────────────────────────────────────────────────────────────────────────
# Checksum + Sigstore
# ───────────────────────────────────────────────────────────────────────────

file_sha256() {
  local file="$1"
  if command -v sha256sum &>/dev/null; then
    sha256sum "$file" | cut -d' ' -f1
  elif command -v shasum &>/dev/null; then
    shasum -a 256 "$file" | cut -d' ' -f1
  else
    err "No SHA256 tool found. Install sha256sum or shasum, or set EE_SKIP_VERIFY=1 to bypass verification."
    return 1
  fi
}

verify_checksum() {
  local file="$1" expected="$2" actual=""
  [ -f "$file" ] || { err "File not found: $file"; return 1; }

  actual=$(file_sha256 "$file") || return 1

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

# Fetch $url to $out, classifying a failure rather than just reporting it.
# Sets EE_FETCH_CLASSIFICATION to:
#   "absent"  - a definitive HTTP 404: the resource genuinely does not exist.
#   "unknown" - anything else (network failure, timeout, DNS failure, or a
#               5xx that survived ee_curl's own internal retries) -- NOT
#               evidence the resource is missing, just that we could not
#               fetch it this run.
# v0.13.1 ships with no SLSA provenance or Sigstore attestation for ANY
# platform, so --require-provenance always fails against it -- but until
# this fix, that failure read identically to a transient network blip. An
# operator (or a client hitting this during a real incident) could not tell
# "this release genuinely lacks provenance, stop retrying" from "the network
# hiccuped, try again" from the message alone. Never let unknown render as
# confirmed absent.
EE_FETCH_CLASSIFICATION=""
fetch_or_classify_absence() {
  local url="$1" out="$2"
  EE_FETCH_CLASSIFICATION="unknown"
  local hdr_file=""
  hdr_file="$(mktemp "${TMPDIR:-/tmp}/ee-fetch-hdr.XXXXXX" 2>/dev/null || true)"
  if [ -z "$hdr_file" ]; then
    # No mktemp available: fetch without headers, cannot classify beyond
    # "unknown" -- honest about the limitation rather than guessing.
    ee_curl "$url" -o "$out" 2>/dev/null && return 0
    return 1
  fi
  if ee_curl "$url" -o "$out" -D "$hdr_file" 2>/dev/null; then
    rm -f "$hdr_file" 2>/dev/null
    return 0
  fi
  local http_status=""
  http_status=$(grep -oE '^HTTP/[0-9.]+ [0-9]{3}' "$hdr_file" 2>/dev/null | tail -1 | awk '{print $2}')
  rm -f "$hdr_file" 2>/dev/null
  [ "$http_status" = "404" ] && EE_FETCH_CLASSIFICATION="absent"
  return 1
}

verify_sigstore_bundle() {
  local file="$1" artifact_url="$2"

  local bundle_url="${artifact_url}.sigstore.json"
  local bundle_file
  bundle_file="$TMP/$(basename "$bundle_url")"

  # Sigstore is best-effort by default: when a release ships without a
  # `.sigstore.json` bundle, we keep the install moving on the strength
  # of the always-on sha256 + GitHub Release upload integrity check.
  # Users who want strict cryptographic-trust enforcement opt in with
  # `--require-provenance`, which separately requires a signed provenance
  # bundle and refuses the install when either is missing or invalid.
  #
  # Manual maintainer-cut releases (cosign keyless device-flow) are
  # operationally expensive and were previously the single hardest UX
  # blocker on the curl|bash path; soft-skipping when a bundle is absent
  # removes that blocker without weakening the security floor (sha256
  # mismatch and bundle-present-but-invalid both still abort).
  info "Fetching sigstore bundle"
  info "Sigstore bundle: $bundle_url" >&2
  if ! fetch_or_classify_absence "$bundle_url" "$bundle_file"; then
    if [ "$REQUIRE_PROVENANCE" = "1" ] || [ "$REQUIRE_KEYLESS" = "1" ]; then
      if [ "$EE_FETCH_CLASSIFICATION" = "absent" ]; then
        err "Sigstore bundle does not exist at $bundle_url (HTTP 404)."
        err "This release genuinely does not publish a signed bundle; strict verification cannot succeed against it."
      else
        err "Could not fetch the Sigstore bundle at $bundle_url (network failure, not a confirmed-absent 404)."
        err "This may be transient rather than evidence the release lacks a signed bundle -- retry, or re-run without --require-provenance."
      fi
      err "Strict verification was requested; cannot continue without a signed bundle."
      return 1
    fi
    if [ "$EE_FETCH_CLASSIFICATION" = "absent" ]; then
      warn "Sigstore bundle not published at $bundle_url; skipping signature verification (sha256 already verified)."
    else
      warn "Could not fetch the Sigstore bundle at $bundle_url (network failure, not confirmed absent); skipping signature verification (sha256 already verified)."
    fi
    warn "Pass --require-provenance to fail the install when a signed bundle is missing."
    return 0
  fi

  if ! command -v cosign &>/dev/null; then
    if [ "$REQUIRE_KEYLESS" = "1" ]; then
      err "cosign not found. EE_INSTALL_REQUIRE_KEYLESS=1 requires keyless Sigstore verification."
      err "Install cosign (https://github.com/sigstore/cosign) or unset EE_INSTALL_REQUIRE_KEYLESS."
      return 1
    fi
    warn "cosign not found; skipping Sigstore signature verification for $file (sha256 already verified)."
    warn "Install cosign (https://github.com/sigstore/cosign) for cryptographic-trust verification."
    return 0
  fi

  info "Sigstore identity regexp: $CERT_IDENTITY_REGEXP" >&2
  info "Sigstore OIDC issuer: $CERT_OIDC_ISSUER" >&2
  if ! verify_blob_against_anchors "$bundle_file" "$file"; then
    err "Sigstore signature verification failed for $file"
    err "A signed bundle was published for this release but did not verify against any trusted anchor:"
    local i
    for i in "${!CERT_IDENTITY_REGEXPS[@]}"; do
      err "  - identity_regexp='${CERT_IDENTITY_REGEXPS[$i]}' issuer='${CERT_OIDC_ISSUERS[$i]}'"
    done
    if [ "$REQUIRE_KEYLESS" = "1" ]; then
      err "Pinned-key fallback was disabled by EE_INSTALL_REQUIRE_KEYLESS=1."
    fi
    err "This is a security signal worth investigating; do not pass --no-verify to bypass."
    return 1
  fi
  ok "Sigstore signature verified"
}

verify_provenance_bundle() {
  local file="$1" artifact_url="$2"
  [ "$REQUIRE_PROVENANCE" = "1" ] || return 0

  if ! command -v cosign &>/dev/null; then
    err "cosign not found. Cannot verify provenance signature."
    err "Install cosign (https://github.com/sigstore/cosign) or re-run without --require-provenance."
    return 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    err "python3 not found. Cannot validate SLSA provenance JSON."
    err "Install python3 or re-run without --require-provenance."
    return 1
  fi

  local provenance_url="${artifact_url%.tar.xz}.provenance.json"
  if [ "$provenance_url" = "$artifact_url" ]; then
    provenance_url="${artifact_url}.provenance.json"
  fi
  local bundle_url="${provenance_url}.sigstore.json"

  local provenance_file bundle_file artifact_sha artifact_name
  provenance_file="$TMP/$(basename "$provenance_url")"
  bundle_file="$TMP/$(basename "$bundle_url")"
  artifact_name="$(basename "$file")"

  info "Fetching SLSA provenance"
  info "Provenance: $provenance_url" >&2
  if ! fetch_or_classify_absence "$provenance_url" "$provenance_file"; then
    if [ "$EE_FETCH_CLASSIFICATION" = "absent" ]; then
      err "Provenance JSON does not exist at $provenance_url (HTTP 404)."
      err "This release genuinely does not publish SLSA provenance for this platform."
    else
      err "Could not fetch provenance JSON from $provenance_url (network failure, not a confirmed-absent 404)."
      err "This may be transient rather than evidence the release lacks provenance -- retry, or re-run without --require-provenance."
    fi
    err "Cannot satisfy --require-provenance."
    return 1
  fi
  if ! fetch_or_classify_absence "$bundle_url" "$bundle_file"; then
    if [ "$EE_FETCH_CLASSIFICATION" = "absent" ]; then
      err "Provenance Sigstore bundle does not exist at $bundle_url (HTTP 404)."
      err "This release genuinely does not publish a signed provenance bundle for this platform."
    else
      err "Could not fetch the provenance Sigstore bundle at $bundle_url (network failure, not a confirmed-absent 404)."
      err "This may be transient rather than evidence the release lacks a signed bundle -- retry, or re-run without --require-provenance."
    fi
    err "Cannot satisfy --require-provenance."
    return 1
  fi

  if ! verify_blob_against_anchors "$bundle_file" "$provenance_file"; then
    err "Provenance Sigstore verification failed for $provenance_file"
    err "Trusted anchors tried:"
    local i
    for i in "${!CERT_IDENTITY_REGEXPS[@]}"; do
      err "  - identity_regexp='${CERT_IDENTITY_REGEXPS[$i]}' issuer='${CERT_OIDC_ISSUERS[$i]}'"
    done
    if [ "$REQUIRE_KEYLESS" = "1" ]; then
      err "Pinned-key fallback was disabled by EE_INSTALL_REQUIRE_KEYLESS=1."
    fi
    return 1
  fi

  artifact_sha=$(file_sha256 "$file") || return 1
  if ! python3 - "$provenance_file" "$artifact_name" "$artifact_sha" <<'PY'
import json
import sys

path, artifact_name, artifact_sha = sys.argv[1:4]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)

if data.get("predicateType") != "https://slsa.dev/provenance/v1":
    raise SystemExit("provenance predicateType is not SLSA v1")

subjects = data.get("subject")
if not isinstance(subjects, list) or not subjects:
    raise SystemExit("provenance subject is missing")
subject = subjects[0]
if subject.get("name") != artifact_name:
    raise SystemExit(f"provenance subject {subject.get('name')!r} does not match {artifact_name!r}")
if subject.get("digest", {}).get("sha256") != artifact_sha:
    raise SystemExit("provenance subject sha256 does not match downloaded artifact")

dependencies = (
    data.get("predicate", {})
    .get("buildDefinition", {})
    .get("resolvedDependencies", [])
)
if not any(dep.get("uri") == "file://Cargo.lock" and dep.get("digest", {}).get("blake3") for dep in dependencies):
    raise SystemExit("provenance is missing Cargo.lock blake3 dependency")
if not any(dep.get("digest", {}).get("gitCommit") for dep in dependencies):
    raise SystemExit("provenance is missing source git commit dependency")
PY
  then
    err "Provenance JSON validation failed"
    return 1
  fi
  ok "SLSA provenance verified"
}

# ───────────────────────────────────────────────────────────────────────────
# Optional semantic first-use smoke
# ───────────────────────────────────────────────────────────────────────────

semantic_smoke_mode() {
  printf '%s' "${EE_INSTALL_SEMANTIC_SMOKE:-0}" | tr '[:upper:]' '[:lower:]'
}

semantic_smoke_enabled() {
  case "$(semantic_smoke_mode)" in
    1|true|yes|warn|warning|require|required|fail) return 0 ;;
    *) return 1 ;;
  esac
}

semantic_smoke_required() {
  case "$(semantic_smoke_mode)" in
    1|true|yes|require|required|fail) return 0 ;;
    *) return 1 ;;
  esac
}

semantic_smoke_fail_or_warn() {
  local message="$1"
  if semantic_smoke_required; then
    err "$message"
    return 1
  fi
  warn "$message"
  return 0
}

run_semantic_first_use_smoke() {
  local bin="$1"
  semantic_smoke_enabled || return 0

  local smoke_ws archive rerank_url fetch_json status_json search_json compact
  local rerank_score_matches reranked_kind_matches
  local observed_rerank_scores observed_reranked_kinds
  smoke_ws="$TMP/semantic-first-use-workspace"
  archive="$TMP/rerank-default-v1.tar.zst"
  rerank_url="${EE_INSTALL_RERANK_MODEL_URL:-https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/rerank-default-v1/rerank-default-v1.tar.zst}"
  if ! mkdir -p "$smoke_ws" 2>/dev/null; then
    semantic_smoke_fail_or_warn "Semantic first-use smoke could not create workspace at $smoke_ws"
    return $?
  fi

  if [ "$OFFLINE" = "1" ]; then
    semantic_smoke_fail_or_warn "Semantic first-use smoke requires network access; --offline was requested"
    return $?
  fi

  info "Running semantic + native-reranker first-use smoke (EE_INSTALL_SEMANTIC_SMOKE=$(semantic_smoke_mode))"
  if ! "$bin" init --workspace "$smoke_ws" --json >/dev/null 2>&1; then
    semantic_smoke_fail_or_warn "Semantic first-use smoke failed during ee init"
    return $?
  fi

  info "Downloading the pinned native reranker for first-use verification"
  if ! ee_curl "$rerank_url" -o "$archive"; then
    semantic_smoke_fail_or_warn "Native-reranker first-use smoke could not download the pinned model archive"
    return $?
  fi

  fetch_json="$TMP/rerank-model-fetch.json"
  if ! "$bin" --workspace "$smoke_ws" \
        model fetch rerank-default --from-file "$archive" --json \
        >"$fetch_json" 2>&1; then
    semantic_smoke_fail_or_warn "Native-reranker first-use smoke failed during ee model fetch"
    return $?
  fi
  compact=$(tr -d '\n' <"$fetch_json")
  if ! printf '%s' "$compact" | grep -Eq '"success"[[:space:]]*:[[:space:]]*true' \
     || ! printf '%s' "$compact" | grep -Eq '"schema"[[:space:]]*:[[:space:]]*"ee.model_fetch.v1"' \
     || ! printf '%s' "$compact" | grep -Eq '"modelId"[[:space:]]*:[[:space:]]*"rerank-default-v1"' \
     || ! printf '%s' "$compact" | grep -Eq '"modelPurpose"[[:space:]]*:[[:space:]]*"reranker"' \
     || ! printf '%s' "$compact" | grep -Eq '"registryEntry"[[:space:]]*:[[:space:]]*\{[^}]*"status"[[:space:]]*:[[:space:]]*"available"'; then
    semantic_smoke_fail_or_warn "Native-reranker first-use smoke model fetch did not return an available reranker"
    return $?
  fi

  while IFS='|' read -r level kind content; do
    if ! "$bin" remember \
          --workspace "$smoke_ws" \
          --level "$level" \
          --kind "$kind" \
          --tags install-smoke,semantic,rerank \
          --no-auto-link \
          --no-propose-candidates \
          --json \
          "$content" \
          >/dev/null 2>&1; then
      semantic_smoke_fail_or_warn "Semantic first-use smoke failed while seeding the reranker corpus"
      return $?
    fi
  done <<'EOF_RERANK_SMOKE'
semantic|fact|EE_INSTALL_RERANK_TRAP release installer model bootstrap release release checklist checklist cargo cargo clippy clippy, but this is noisy release prose rather than the policy target.
procedural|rule|EE_INSTALL_RERANK_TARGET release installer model bootstrap: run cargo fmt --check and cargo clippy before publishing a Rust release.
semantic|fact|EE_INSTALL_RERANK_NOISE_ONE release installer model bootstrap evidence also tracks database migration ordering and index ownership.
semantic|fact|EE_INSTALL_RERANK_NOISE_TWO release installer model bootstrap evidence includes onboarding screenshots and terminal theme review.
semantic|fact|EE_INSTALL_RERANK_NOISE_THREE release installer model bootstrap evidence notes that Rust ownership prevents memory-safety errors.
EOF_RERANK_SMOKE

  if ! "$bin" index rebuild --workspace "$smoke_ws" --json >/dev/null 2>&1; then
    semantic_smoke_fail_or_warn "Semantic first-use smoke failed during ee index rebuild"
    return $?
  fi
  if ! "$bin" config set search.rerank_top_k 5 \
        --workspace "$smoke_ws" --json >/dev/null 2>&1; then
    semantic_smoke_fail_or_warn "Native-reranker first-use smoke failed to set rerank_top_k"
    return $?
  fi
  if ! "$bin" config set search.rerank auto \
        --workspace "$smoke_ws" --json >/dev/null 2>&1; then
    semantic_smoke_fail_or_warn "Native-reranker first-use smoke failed to enable rerank auto mode"
    return $?
  fi
  if ! status_json=$("$bin" model status --workspace "$smoke_ws" --json 2>&1); then
    semantic_smoke_fail_or_warn "Semantic first-use smoke failed during ee model status: $status_json"
    return $?
  fi

  compact=$(printf '%s' "$status_json" | tr -d '\n')
  if ! printf '%s' "$compact" | grep -Eq '"semanticReadiness"[[:space:]]*:[[:space:]]*\{[^}]*"state"[[:space:]]*:[[:space:]]*"available"[^}]*"mode"[[:space:]]*:[[:space:]]*"semantic"'; then
    semantic_smoke_fail_or_warn "Semantic first-use smoke did not reach semanticReadiness.state=available mode=semantic"
    return $?
  fi

  if ! search_json=$("$bin" search \
        "release installer model bootstrap cargo formatting before publishing" \
        --workspace "$smoke_ws" \
        --limit 5 \
        --relevance-floor 0 \
        --explain \
        --json 2>&1); then
    semantic_smoke_fail_or_warn "Native-reranker first-use smoke failed during ee search"
    return $?
  fi
  compact=$(printf '%s' "$search_json" | tr -d '\n')
  rerank_score_matches=$(printf '%s' "$compact" \
    | grep -Eo '"rerankScore"[[:space:]]*:[[:space:]]*[-+0-9.eE]+' || true)
  reranked_kind_matches=$(printf '%s' "$compact" \
    | grep -Eo '"scoreKind"[[:space:]]*:[[:space:]]*"reranked"' || true)
  observed_rerank_scores=$(printf '%s\n' "$rerank_score_matches" \
    | awk 'NF { count += 1 } END { print count + 0 }')
  observed_reranked_kinds=$(printf '%s\n' "$reranked_kind_matches" \
    | awk 'NF { count += 1 } END { print count + 0 }')
  if ! printf '%s' "$compact" | grep -Eq '"success"[[:space:]]*:[[:space:]]*true' \
     || ! printf '%s' "$compact" | grep -Eq '"schema"[[:space:]]*:[[:space:]]*"ee.rerank_posture.v1"' \
     || ! printf '%s' "$compact" | grep -Eq '"mode"[[:space:]]*:[[:space:]]*"reranked"' \
     || ! printf '%s' "$compact" | grep -Eq '"configured"[[:space:]]*:[[:space:]]*"auto"' \
     || ! printf '%s' "$compact" | grep -Eq '"available"[[:space:]]*:[[:space:]]*true' \
     || ! printf '%s' "$compact" | grep -Eq '"returnedCount"[[:space:]]*:[[:space:]]*5[,}]' \
     || ! printf '%s' "$compact" | grep -Eq '"resultCount"[[:space:]]*:[[:space:]]*5[,}]' \
     || ! printf '%s' "$compact" | grep -Eq '"rerankScoreCount"[[:space:]]*:[[:space:]]*5[,}]' \
     || [ "$observed_rerank_scores" -ne 5 ] \
     || [ "$observed_reranked_kinds" -ne 6 ] \
     || ! printf '%s\n' "$rerank_score_matches" | awk -F: \
          'NF { value = $NF + 0; if (!(value > 0 && value <= 1)) exit 1; count += 1 } END { if (count != 5) exit 1 }' \
     || printf '%s' "$compact" | grep -q 'rerank_model_unavailable'; then
    semantic_smoke_fail_or_warn "Native-reranker first-use smoke did not produce five model-backed reranked results"
    return $?
  fi

  ok "Semantic + native-reranker first-use smoke passed"
}

# ───────────────────────────────────────────────────────────────────────────
# Build-from-source helpers
# ───────────────────────────────────────────────────────────────────────────

clone_source_tree() {
  local destination="$1"
  local repository_url="https://github.com/${OWNER}/${REPO}.git"

  if [ -z "$VERSION" ]; then
    git clone --depth 1 "$repository_url" "$destination"
    return
  fi

  # A requested release is an immutable identity boundary. Never turn a
  # misspelled/missing tag into a successful build of the default branch:
  # callers would believe they installed VERSION while receiving unrelated
  # code. Clone only that ref, then prove both that it is a tag and that HEAD
  # resolves to the tag's commit (including annotated tags).
  if ! git clone --depth 1 --branch "$VERSION" --single-branch \
        "$repository_url" "$destination"; then
    err "Could not clone requested release tag $VERSION."
    err "Refusing to build a different revision; verify the version and try again."
    return 1
  fi

  local requested_commit=""
  local head_commit=""
  if ! requested_commit=$(git -C "$destination" rev-parse --verify \
        "refs/tags/${VERSION}^{commit}" 2>/dev/null) || [ -z "$requested_commit" ]; then
    err "Requested source revision $VERSION is not an exact release tag."
    err "Refusing to build a branch or different revision."
    return 1
  fi
  if ! head_commit=$(git -C "$destination" rev-parse --verify HEAD 2>/dev/null) \
     || [ -z "$head_commit" ] || [ "$head_commit" != "$requested_commit" ]; then
    err "Source checkout for $VERSION does not match its release tag."
    err "Refusing to build an unverified revision."
    return 1
  fi
}

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

select_extracted_binary() {
  local candidate
  local executable_candidates=()
  local all_candidates=()

  # NOTE: -perm -u+x (owner-execute) rather than -perm -111 — macOS tarballs
  # historically ship the binary with mode 700, so requiring g+x/o+x falsely
  # rejects a valid binary (see issue #4).
  while IFS= read -r candidate; do
    [ -n "$candidate" ] && executable_candidates+=("$candidate")
  done < <(find "$TMP/extract" -maxdepth 3 -type f -name "$BINARY" -perm -u+x 2>/dev/null | LC_ALL=C sort)

  if [ "${#executable_candidates[@]}" -eq 1 ]; then
    BIN="${executable_candidates[0]}"
    return 0
  fi

  if [ "${#executable_candidates[@]}" -gt 1 ]; then
    err "Archive contains multiple executable '$BINARY' candidates:"
    for candidate in "${executable_candidates[@]+"${executable_candidates[@]}"}"; do
      err "  - ${candidate#"$TMP"/extract/}"
    done
    err "Refusing to choose by filesystem traversal order."
    return 1
  fi

  # Fallback for archives that carry the right file but omit owner-execute mode:
  # accept exactly one candidate, log the repair, and fail loudly if chmod fails.
  while IFS= read -r candidate; do
    [ -n "$candidate" ] && all_candidates+=("$candidate")
  done < <(find "$TMP/extract" -maxdepth 3 -type f -name "$BINARY" 2>/dev/null | LC_ALL=C sort)

  if [ "${#all_candidates[@]}" -eq 0 ]; then
    err "Binary '$BINARY' not found in archive after extraction"
    return 1
  fi

  if [ "${#all_candidates[@]}" -gt 1 ]; then
    err "Archive contains multiple matching '$BINARY' candidates without owner-execute mode:"
    for candidate in "${all_candidates[@]+"${all_candidates[@]}"}"; do
      err "  - ${candidate#"$TMP"/extract/}"
    done
    err "Refusing to choose by filesystem traversal order."
    return 1
  fi

  BIN="${all_candidates[0]}"
  warn "Extracted '$BINARY' lacks owner-execute mode; applying chmod u+x to ${BIN#"$TMP"/extract/}"
  if ! chmod u+x "$BIN" 2>/dev/null; then
    err "Binary '$BINARY' found but chmod u+x failed: ${BIN#"$TMP"/extract/}"
    return 1
  fi
  if [ ! -x "$BIN" ]; then
    err "Binary '$BINARY' found but is still not executable after chmod: ${BIN#"$TMP"/extract/}"
    return 1
  fi
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
if [ "$REQUIRE_PROVENANCE" = "1" ] && [ "$FROM_SOURCE" -eq 1 ]; then
  err "--require-provenance only applies to signed release artifacts, not --from-source builds"
  exit 2
fi
if [ "$REQUIRE_KEYLESS" = "1" ] && [ "$FROM_SOURCE" -eq 1 ]; then
  err "EE_INSTALL_REQUIRE_KEYLESS=1 only applies to signed release artifacts, not --from-source builds"
  exit 2
fi

# Ensure the destination dir exists before write-perm check.
mkdir -p "$DEST" 2>/dev/null || true

preflight_checks

# Already-installed short-circuit. Acquisition and locking stay skipped, but
# idempotent shell integration and explicitly requested verification still run.
if [ "$FROM_SOURCE" -eq 0 ] && [ "$FORCE_INSTALL" -eq 0 ] && [ -n "$VERSION" ] \
   && check_installed_version "$VERSION"; then
  ok "$PROJECT_LABEL $VERSION is already installed at $DEST/$BINARY"
  info "Use --force to reinstall"
  maybe_add_path
  maybe_install_completions
  if [ "$VERIFY" -eq 1 ]; then
    run_install_self_test || exit 1
  fi
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
  if ! download_release_artifact; then
    if [ "$REQUIRE_PROVENANCE" = "1" ]; then
      err "Artifact download failed and --require-provenance forbids source fallback"
      exit 1
    fi
    if [ -n "$ARTIFACT_URL" ] || [ -n "$CHECKSUM" ] || [ -n "$CHECKSUM_URL" ]; then
      err "Artifact download failed; explicit artifact/checksum inputs forbid automatic retargeting or source fallback"
      exit 1
    fi
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

  if ! clone_source_tree "$TMP/src"; then
    exit 1
  fi

  info "Checking out locked Franken-stack source dependencies"
  "$TMP/src/scripts/checkout-franken-stack.sh" "$TMP"

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
    if ! verify_provenance_bundle "$TMP/$TAR" "$URL"; then
      err "Installation aborted: provenance verification failed"
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

  BIN=""
  if ! select_extracted_binary; then
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
  run_install_self_test || exit 1
fi

run_semantic_first_use_smoke "$DEST/$BINARY"

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
Before substantial work:
  ee pack \"<task>\" --workspace . --max-tokens 4000 --format markdown

Before editing a known path:
  ee recall --path <path> --workspace . --budget-tokens 400 --format markdown"
    echo ""
  fi

  if is_agent_detected "codex-cli"; then
    print_integration_snippet "Codex CLI (~/.codex/AGENTS.md)" "\
Before substantial work:
  ee pack \"<task>\" --workspace . --json"
    echo ""
  fi

  if is_agent_detected "gemini-cli"; then
    print_integration_snippet "Gemini CLI (~/.gemini/GEMINI.md or settings.json)" "\
Before substantial work:
  ee pack \"<task>\" --workspace . --json"
    echo ""
  fi

  if is_agent_detected "cursor-ide"; then
    print_integration_snippet "Cursor IDE (~/.cursor/hooks.json)" "\
Before substantial work:
  ee pack \"<task>\" --workspace . --json"
    echo ""
  fi

  if is_agent_detected "aider" || is_agent_detected "continue" || is_agent_detected "github-copilot-cli"; then
    print_integration_snippet "Aider / Continue / Copilot CLI" "\
Call ee directly from your prompt setup:
  ee pack \"<task>\" --workspace . --json"
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
    gum style --foreground 39  "  ee pack \"<task>\" --workspace . --max-tokens 4000"
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
  echo -e "    \033[0;34mee pack \"<task>\" --workspace . --max-tokens 4000\033[0m"
  echo -e "    \033[0;34mee --help\033[0m"
  echo ""
  echo -e "  \033[0;90mInspect health:  ee doctor --json\033[0m"
  echo -e "  \033[0;90mUninstall:       rm $DEST/$BINARY  (config in ~/.ee/ and ~/.local/share/ee/ persists)\033[0m"
fi
