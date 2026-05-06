#!/bin/sh
set -eu

# Update Homebrew formula for ee release (eidetic_engine_cli-4e56)
#
# This script:
# 1. Reads the template from scripts/homebrew/ee.rb.template
# 2. Downloads release artifacts to get SHA256 hashes
# 3. Substitutes placeholders with actual values
# 4. Outputs the formula to stdout (or file if specified)
#
# Usage:
#   ./scripts/homebrew/update-formula.sh <version> [output-file]
#
# Example:
#   ./scripts/homebrew/update-formula.sh 0.1.0 > Formula/ee.rb
#   ./scripts/homebrew/update-formula.sh 0.1.0 /path/to/homebrew-tap/Formula/ee.rb

if [ $# -lt 1 ]; then
    echo "Usage: $0 <version> [output-file]" >&2
    echo "Example: $0 0.1.0" >&2
    exit 1
fi

VERSION="$1"
OUTPUT_FILE="${2:-}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TEMPLATE="$SCRIPT_DIR/ee.rb.template"
REPO="Dicklesworthstone/eidetic_engine_cli"
RELEASE_URL="https://github.com/$REPO/releases/download/v$VERSION"

if [ ! -f "$TEMPLATE" ]; then
    echo "Error: Template not found at $TEMPLATE" >&2
    exit 1
fi

echo "Fetching SHA256 hashes for v$VERSION..." >&2

# Fetch SHA256 files from release
DARWIN_ARM64_SHA=""
DARWIN_X86_64_SHA=""
LINUX_X86_64_SHA=""

for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu; do
    SHA_URL="$RELEASE_URL/ee-$target.tar.xz.sha256"
    echo "  Fetching $SHA_URL" >&2
    SHA=$(curl -sL "$SHA_URL" 2>/dev/null | awk '{print $1}' || echo "")

    case "$target" in
        aarch64-apple-darwin) DARWIN_ARM64_SHA="$SHA" ;;
        x86_64-apple-darwin) DARWIN_X86_64_SHA="$SHA" ;;
        x86_64-unknown-linux-gnu) LINUX_X86_64_SHA="$SHA" ;;
    esac
done

if [ -z "$DARWIN_ARM64_SHA" ] || [ -z "$DARWIN_X86_64_SHA" ] || [ -z "$LINUX_X86_64_SHA" ]; then
    echo "Error: Could not fetch all SHA256 hashes" >&2
    echo "  DARWIN_ARM64_SHA: ${DARWIN_ARM64_SHA:-missing}" >&2
    echo "  DARWIN_X86_64_SHA: ${DARWIN_X86_64_SHA:-missing}" >&2
    echo "  LINUX_X86_64_SHA: ${LINUX_X86_64_SHA:-missing}" >&2
    exit 1
fi

echo "Generating formula..." >&2

# Substitute placeholders
FORMULA=$(cat "$TEMPLATE" \
    | sed "s/{{VERSION}}/$VERSION/g" \
    | sed "s/{{SHA256_DARWIN_ARM64}}/$DARWIN_ARM64_SHA/g" \
    | sed "s/{{SHA256_DARWIN_X86_64}}/$DARWIN_X86_64_SHA/g" \
    | sed "s/{{SHA256_LINUX_X86_64}}/$LINUX_X86_64_SHA/g")

if [ -n "$OUTPUT_FILE" ]; then
    echo "$FORMULA" > "$OUTPUT_FILE"
    echo "Formula written to $OUTPUT_FILE" >&2
else
    echo "$FORMULA"
fi
