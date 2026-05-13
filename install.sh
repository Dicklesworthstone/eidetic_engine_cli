#!/bin/sh
# ee (Eidetic Engine CLI) installer
# Usage: curl -fsSL https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.sh | EE_VERSION=v0.1.0 sh
#
# Environment variables:
#   EE_VERSION    - specific version to install (default: latest)
#   EE_INSTALL_DIR - installation directory (default: ~/.local/bin)
#   EE_SKIP_VERIFY - set to 1 to skip SHA256 verification (not recommended)

set -eu

REPO="Dicklesworthstone/eidetic_engine_cli"
GITHUB_API="https://api.github.com/repos/${REPO}/releases"
GITHUB_DL="https://github.com/${REPO}/releases/download"
DEFAULT_INSTALL_DIR="${HOME}/.local/bin"

# Colors (disabled if not a terminal)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

info() { printf "${BLUE}info:${NC} %s\n" "$1"; }
warn() { printf "${YELLOW}warn:${NC} %s\n" "$1" >&2; }
error() { printf "${RED}error:${NC} %s\n" "$1" >&2; exit 1; }
success() { printf "${GREEN}success:${NC} %s\n" "$1"; }

# Detect OS and architecture
detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "${OS}" in
        Linux)
            # Check for musl vs glibc
            if ldd --version 2>&1 | grep -q musl; then
                OS_SUFFIX="unknown-linux-musl"
            else
                OS_SUFFIX="unknown-linux-gnu"
            fi
            ;;
        Darwin)
            OS_SUFFIX="apple-darwin"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            OS_SUFFIX="pc-windows-msvc"
            ;;
        *)
            error "Unsupported operating system: ${OS}"
            ;;
    esac

    case "${ARCH}" in
        x86_64|amd64)
            ARCH_PREFIX="x86_64"
            ;;
        aarch64|arm64)
            ARCH_PREFIX="aarch64"
            ;;
        *)
            error "Unsupported architecture: ${ARCH}"
            ;;
    esac

    TARGET="${ARCH_PREFIX}-${OS_SUFFIX}"
    info "Detected platform: ${TARGET}"
}

# HTTP client abstraction (curl or wget)
http_get() {
    URL="$1"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$URL"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- "$URL"
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

http_download() {
    URL="$1"
    OUTPUT="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$OUTPUT" "$URL"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$OUTPUT" "$URL"
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

# Get latest version from GitHub API
get_latest_version() {
    info "Fetching latest release..."
    LATEST=$(http_get "${GITHUB_API}/latest" | grep '"tag_name"' | head -1 | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
    if [ -z "${LATEST}" ]; then
        error "Failed to determine latest version. Check your network connection."
    fi
    echo "${LATEST}"
}

# Verify SHA256 checksum
verify_sha256() {
    FILE="$1"
    EXPECTED="$2"

    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL=$(sha256sum "$FILE" | cut -d' ' -f1)
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL=$(shasum -a 256 "$FILE" | cut -d' ' -f1)
    else
        error "No SHA256 tool found. Install sha256sum or shasum, or set EE_SKIP_VERIFY=1 to bypass verification."
    fi

    if [ "$ACTUAL" != "$EXPECTED" ]; then
        error "SHA256 mismatch!\n  Expected: ${EXPECTED}\n  Actual:   ${ACTUAL}\nThe download may be corrupted or tampered with."
    fi
    info "SHA256 verified: ${ACTUAL}"
}

# Verify Sigstore signature (optional)
verify_sigstore() {
    BUNDLE="$1"
    ARTIFACT="$2"

    if ! command -v cosign >/dev/null 2>&1; then
        warn "cosign not found. Skipping Sigstore verification."
        warn "Install cosign for cryptographic signature verification."
        return 0
    fi

    info "Verifying Sigstore signature..."
    if cosign verify-blob --bundle "$BUNDLE" "$ARTIFACT" >/dev/null 2>&1; then
        info "Sigstore signature verified."
    else
        error "Sigstore verification failed."
    fi
}

# Main installation
main() {
    info "ee (Eidetic Engine CLI) installer"

    # Detect platform
    detect_platform

    # Determine version
    VERSION="${EE_VERSION:-}"
    if [ -z "${VERSION}" ]; then
        VERSION=$(get_latest_version)
    fi
    case "${VERSION}" in
        v*) ;;
        *) VERSION="v${VERSION}" ;;
    esac
    info "Installing version: ${VERSION}"

    # Determine install directory
    INSTALL_DIR="${EE_INSTALL_DIR:-${DEFAULT_INSTALL_DIR}}"
    info "Install directory: ${INSTALL_DIR}"

    # Create temp directory
    TMPDIR=$(mktemp -d)
    trap 'rm -rf "${TMPDIR}"' EXIT

    # Construct URLs
    TARBALL="ee-${TARGET}.tar.xz"
    TARBALL_URL="${GITHUB_DL}/${VERSION}/${TARBALL}"
    SHA256_URL="${TARBALL_URL}.sha256"
    SIGSTORE_URL="${TARBALL_URL}.sigstore.json"

    # Download tarball
    info "Downloading ${TARBALL}..."
    http_download "${TARBALL_URL}" "${TMPDIR}/${TARBALL}" || \
        error "Failed to download ${TARBALL_URL}"

    # Download and verify SHA256
    if [ "${EE_SKIP_VERIFY:-0}" != "1" ]; then
        info "Downloading checksum..."
        EXPECTED_SHA=$(http_get "${SHA256_URL}" | cut -d' ' -f1) || \
            error "Failed to download checksum"
        verify_sha256 "${TMPDIR}/${TARBALL}" "${EXPECTED_SHA}"

        # Sigstore verification is required when cosign is installed.
        if command -v cosign >/dev/null 2>&1; then
            http_download "${SIGSTORE_URL}" "${TMPDIR}/sigstore.json" || \
                error "Failed to download Sigstore bundle"
            verify_sigstore "${TMPDIR}/sigstore.json" "${TMPDIR}/${TARBALL}"
        else
            warn "cosign not found. Skipping Sigstore verification."
            warn "Install cosign for cryptographic signature verification."
        fi
    else
        warn "Skipping verification (EE_SKIP_VERIFY=1)"
    fi

    # Extract
    info "Extracting..."
    mkdir -p "${TMPDIR}/extract"
    if command -v xz >/dev/null 2>&1; then
        xz -dc "${TMPDIR}/${TARBALL}" | tar -xf - -C "${TMPDIR}/extract"
    elif command -v unxz >/dev/null 2>&1; then
        unxz -c "${TMPDIR}/${TARBALL}" | tar -xf - -C "${TMPDIR}/extract"
    else
        # Try tar with xz support
        tar -xJf "${TMPDIR}/${TARBALL}" -C "${TMPDIR}/extract" 2>/dev/null || \
            error "xz decompression not available. Install xz-utils."
    fi

    # Find the binary
    BINARY=$(find "${TMPDIR}/extract" -name "ee" -type f | head -1)
    if [ -z "${BINARY}" ]; then
        error "Binary 'ee' not found in archive"
    fi

    # Install
    mkdir -p "${INSTALL_DIR}"
    cp "${BINARY}" "${INSTALL_DIR}/ee"
    chmod +x "${INSTALL_DIR}/ee"
    info "Installed to ${INSTALL_DIR}/ee"

    # Verify installation
    if "${INSTALL_DIR}/ee" --version >/dev/null 2>&1; then
        success "ee installed successfully!"
        "${INSTALL_DIR}/ee" --version
    else
        error "Installation verification failed"
    fi

    # Run doctor if available
    if "${INSTALL_DIR}/ee" doctor --json >/dev/null 2>&1; then
        info "ee doctor: healthy"
    fi

    # PATH instructions
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*)
            ;;
        *)
            echo ""
            warn "${INSTALL_DIR} is not in your PATH"
            echo "Add this to your shell profile (~/.bashrc, ~/.zshrc, etc.):"
            echo ""
            echo "  export PATH=\"\${PATH}:${INSTALL_DIR}\""
            echo ""
            ;;
    esac

    success "Installation complete!"
}

main "$@"
