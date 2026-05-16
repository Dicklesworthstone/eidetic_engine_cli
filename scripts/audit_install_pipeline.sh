#!/usr/bin/env bash
# bd-3usjw.9 — install pipeline audit (read-only release-readiness inventory).
#
# Probes the three install paths advertised in README:
#
#   - curl-pipe install.sh + Sigstore-verified bundle
#   - brew install Dicklesworthstone/tap/ee
#   - cargo install eidetic-engine from crates.io
#
# Emits `ee.audit.install_pipeline.v1` JSON to
# `tests/audit_artifacts/install_pipeline_<UTC>.json`. CI consumers
# (and the README "shipped vs planned" reconciliation) read it.
#
# Phase 1 is read-only — no installs, no network mutations. If a
# probe fails (no network, no gh CLI, etc.), the corresponding field
# is set to `null` with a `*_probe_status` sibling string explaining
# why. The audit always exits 0 on a successful run; exit 1 means
# the audit itself broke (missing required tool, write failure).

# shellcheck disable=SC2016 # This audit intentionally greps literal shell/GitHub expressions.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATES_IO_PACKAGE_NAME="eidetic-engine"
TIMESTAMP="$(date -u +%Y-%m-%dT%H%M%SZ)"
OUTPUT_DIR="$REPO_ROOT/tests/audit_artifacts"
OUTPUT="${1:-$OUTPUT_DIR/install_pipeline_${TIMESTAMP}.json}"
LATEST_SYMLINK="$OUTPUT_DIR/latest_install_pipeline.json"

mkdir -p "$OUTPUT_DIR"

# Required tools.
for tool in jq git; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "audit_install_pipeline: required tool '$tool' not found in PATH" >&2
        exit 2
    fi
done

ISO_TIMESTAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
SRC_COMMIT="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo 'no-commit')"

# --- Probe 1: gh release list (head tags + their assets) ----------
gh_releases() {
    if ! command -v gh >/dev/null 2>&1; then
        echo 'gh_cli_missing'
        return
    fi
    # `gh release list` exits 0 with empty output on a repo with no
    # releases. We capture both cases.
    local out
    if out="$(gh -R Dicklesworthstone/eidetic_engine_cli release list \
        --exclude-drafts \
        --exclude-pre-releases \
        --limit 5 \
        --json tagName,isLatest,isDraft,isPrerelease,publishedAt,name 2>&1)"; then
        printf '%s' "$out"
    else
        # gh might exit non-zero on auth issues / network errors.
        echo 'gh_probe_failed'
        return
    fi
}

# --- Probe 2: crates.io for the selected package -------------------
crates_io_inventory() {
    if ! command -v curl >/dev/null 2>&1; then
        echo 'curl_missing'
        return
    fi
    local response
    if response="$(curl -fsSL --max-time 10 \
        "https://crates.io/api/v1/crates/${CRATES_IO_PACKAGE_NAME}" 2>&1)"; then
        printf '%s' "$response"
    else
        echo 'crates_io_unreachable'
    fi
}

# --- Probe 3: Homebrew tap formula --------------------------------
brew_tap_inventory() {
    if ! command -v curl >/dev/null 2>&1; then
        echo 'curl_missing'
        return
    fi
    local response
    if response="$(curl -fsSL --max-time 10 \
        'https://raw.githubusercontent.com/Dicklesworthstone/homebrew-tap/main/Formula/ee.rb' 2>&1)"; then
        printf '%s' "$response"
    else
        echo 'tap_unreachable'
    fi
}

# --- Probe 4: crates.io dependency resolution ----------------------
dependency_resolution_inventory() {
    if ! command -v curl >/dev/null 2>&1; then
        jq -n '{
            probe_status: "curl_missing",
            dep_resolution_ready: null,
            unresolved_count: null,
            required_crates: []
        }'
        return
    fi

    local results='[]'
    local crate_name version role source strategy response status repository latest_version item
    while IFS='|' read -r crate_name version role source strategy; do
        [ -n "$crate_name" ] || continue
        status='unreachable'
        repository=''
        latest_version=''
        if response="$(curl -fsSL --max-time 10 \
            "https://crates.io/api/v1/crates/${crate_name}" 2>/dev/null)"; then
            repository="$(printf '%s' "$response" | jq -r '.crate.repository // ""')"
            latest_version="$(printf '%s' "$response" | jq -r '.crate.newest_version // ""')"
            if printf '%s' "$response" | jq -e --arg version "$version" \
                '.versions[]? | select(.num == $version and (.yanked | not))' >/dev/null 2>&1; then
                status='version_available'
            else
                status='crate_exists_version_missing'
            fi
        fi
        item="$(jq -n \
            --arg crate_name "$crate_name" \
            --arg version "$version" \
            --arg role "$role" \
            --arg source "$source" \
            --arg strategy "$strategy" \
            --arg status "$status" \
            --arg repository "$repository" \
            --arg latest_version "$latest_version" \
            '{
                crate_name: $crate_name,
                required_version: $version,
                role: $role,
                source: $source,
                strategy: $strategy,
                status: $status,
                latest_version: $latest_version,
                repository: $repository
            }')"
        results="$(jq -c --argjson item "$item" '. + [$item]' <<<"$results")"
    done <<'EOF'
asupersync|0.3.1|direct|Cargo.toml dependencies|must_be_published
franken-agent-detection|0.1.3|direct|Cargo.toml dependencies|must_be_published
frankensearch|0.3.0|direct|Cargo.toml dependencies|must_be_published
fnx-algorithms|0.1.0|direct|Cargo.toml dependencies|graph_feature_or_publish
fnx-classes|0.1.0|direct|Cargo.toml dependencies|graph_feature_or_publish
fnx-runtime|0.1.0|direct|Cargo.toml dependencies|graph_feature_or_publish
sqlmodel-core|0.2.2|direct|Cargo.toml dependencies|must_be_published
sqlmodel-frankensqlite|0.2.2|direct|Cargo.toml dependencies|must_be_published
tru|0.2.2|direct|Cargo.toml dependencies|must_be_published
fsqlite|0.1.2|transitive|sqlmodel-frankensqlite dependency|must_be_published
fsqlite-core|0.1.2|transitive|sqlmodel-frankensqlite dependency|must_be_published
fsqlite-error|0.1.2|transitive|sqlmodel-frankensqlite dependency|must_be_published
fsqlite-types|0.1.2|transitive|sqlmodel-frankensqlite dependency|must_be_published
fsqlite-func|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-ext-fts5|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-ext-json|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-ast|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-btree|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-pager|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-parser|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-planner|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-vdbe|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-vfs|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-wal|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-mvcc|0.1.2|transitive|frankensqlite feature surface|must_be_published
fsqlite-observability|0.1.2|transitive|frankensqlite feature surface|must_be_published
frankensearch-core|0.2.0|transitive|frankensearch 0.3.0 workspace dependency|must_be_published
frankensearch-embed|0.2.0|transitive|frankensearch 0.3.0 workspace dependency|must_be_published
frankensearch-index|0.2.0|transitive|frankensearch 0.3.0 workspace dependency|must_be_published
fnx-cgse|0.1.0|transitive|franken_networkx feature surface|graph_feature_or_publish
fnx-convert|0.1.0|transitive|franken_networkx feature surface|graph_feature_or_publish
EOF

    jq -n --argjson crates "$results" '{
        probe_status: "ok",
        dep_resolution_ready: (all($crates[]; .status == "version_available")),
        unresolved_count: ([$crates[] | select(.status != "version_available")] | length),
        required_count: ($crates | length),
        required_crates: $crates
    }'
}

# --- Probe 5: parse release.yml for the asset matrix ---------------
release_workflow_assets() {
    local yml="$REPO_ROOT/.github/workflows/release.yml"
    if [ ! -f "$yml" ]; then
        echo 'no_release_yml'
        return
    fi
    # Pattern-match the matrix targets and the assets each job emits.
    # We do a low-fidelity grep here — the audit reports what we find;
    # detailed validation belongs to PATH-A smoke tests when releases
    # exist.
    {
        echo '{'
        echo '  "path": ".github/workflows/release.yml",'
        # Targets from the matrix block.
        local targets
        targets="$(grep -oE 'target: [a-z0-9_]+-[a-z0-9_-]+-[a-z0-9_]+(-[a-z0-9_]+)?(-msvc)?' "$yml" \
            | sed 's/^target: //' \
            | sort -u \
            | tr '\n' ',' \
            | sed 's/,$//')"
        printf '  "targets_csv": "%s",\n' "${targets:-unknown}"
        # Asset categories (existence, not exhaustive listing).
        printf '  "asset_categories": ['
        local first=1
        for category in 'tar.xz' 'tar.xz.sha256' 'sigstore' 'install.sh' 'install.ps1'; do
            local pattern
            case "$category" in
                'tar.xz') pattern='ee-.*\.tar\.xz' ;;
                'tar.xz.sha256') pattern='\.tar\.xz\.sha256' ;;
                'sigstore') pattern='sigstore' ;;
                'install.sh') pattern='install\.sh' ;;
                'install.ps1') pattern='install\.ps1' ;;
            esac
            if grep -qE "$pattern" "$yml" 2>/dev/null; then
                [ $first -eq 1 ] || printf ','
                printf '"%s"' "$category"
                first=0
            fi
        done
        echo '],'
        # cosign / sigstore presence.
        if grep -qE 'sigstore/cosign-installer' "$yml" 2>/dev/null; then
            echo '  "cosign_installer_present": true'
        else
            echo '  "cosign_installer_present": false'
        fi
        echo ','
        if grep -qE 'tags:' "$yml" 2>/dev/null && grep -qE "['\"]?v\\*['\"]?" "$yml" 2>/dev/null; then
            echo '  "tag_trigger_present": true'
        else
            echo '  "tag_trigger_present": false'
        fi
        echo ','
        if grep -qE -- '--bundle[[:space:]]+ee-.*\.tar\.xz\.sigstore\.json' "$yml" 2>/dev/null; then
            echo '  "cosign_bundle_present": true'
        else
            echo '  "cosign_bundle_present": false'
        fi
        echo ','
        if grep -qF -- '--certificate-identity-regexp "$CERT_IDENTITY_REGEXP"' "$yml" 2>/dev/null \
            && grep -qF -- '--certificate-oidc-issuer "$CERT_OIDC_ISSUER"' "$yml" 2>/dev/null \
            && grep -qF 'https://token.actions.githubusercontent.com' "$yml" 2>/dev/null; then
            echo '  "cosign_identity_bound": true'
        else
            echo '  "cosign_identity_bound": false'
        fi
        echo ','
        if grep -qF 'Verify Sigstore bundles' "$yml" 2>/dev/null \
            && grep -qF 'Missing Sigstore bundle' "$yml" 2>/dev/null \
            && grep -qF 'for artifact in release/ee-*.tar.xz' "$yml" 2>/dev/null; then
            echo '  "release_verifies_sigstore_before_publish": true'
        else
            echo '  "release_verifies_sigstore_before_publish": false'
        fi
        echo ','
        local smoke_containers_json
        smoke_containers_json="$(grep -oE 'image: (ubuntu:24\.04|debian:bookworm-slim)' "$yml" 2>/dev/null \
            | sed 's/^image: //' \
            | sort -u \
            | jq -R . \
            | jq -s .)"
        printf '  "smoke_containers": %s,\n' "${smoke_containers_json:-[]}"
        if grep -qE '^  smoke-test-macos:' "$yml" 2>/dev/null \
            && grep -qE 'runs-on: macos-14' "$yml" 2>/dev/null; then
            echo '  "macos_smoke_present": true'
        else
            echo '  "macos_smoke_present": false'
        fi
        echo ','
        if grep -qE 'EE_VERSION=.*needs\.tag\.outputs\.tag' "$yml" 2>/dev/null; then
            echo '  "smoke_uses_release_tag": true'
        else
            echo '  "smoke_uses_release_tag": false'
        fi
        echo ','
        if grep -qE 'releases/download/\$\{\{ needs\.tag\.outputs\.tag \}\}/install\.sh' "$yml" 2>/dev/null; then
            echo '  "smoke_uses_release_installer_asset": true'
        else
            echo '  "smoke_uses_release_installer_asset": false'
        fi
        echo ','
        if grep -qF 'releases/download/%s/install.sh' "$yml" 2>/dev/null \
            && grep -qF 'releases/download/%s/install.ps1' "$yml" 2>/dev/null; then
            echo '  "release_notes_use_release_assets": true'
        else
            echo '  "release_notes_use_release_assets": false'
        fi
        echo ','
        if grep -qF 'EE_VERSION="%s" sh' "$yml" 2>/dev/null \
            && grep -qF -- '-Version "%s"' "$yml" 2>/dev/null; then
            echo '  "release_notes_pin_installer_version": true'
        else
            echo '  "release_notes_pin_installer_version": false'
        fi
        echo ','
        if grep -qF '} > release_notes.md' "$yml" 2>/dev/null \
            && ! grep -qF 'cat > release_notes.md << NOTES_EOF' "$yml" 2>/dev/null; then
            echo '  "release_notes_no_expanding_heredoc": true'
        else
            echo '  "release_notes_no_expanding_heredoc": false'
        fi
        echo ','
        if grep -qF 'git log -50 --pretty=format:"- %s" "${PREV_TAG}..HEAD"' "$yml" 2>/dev/null \
            && grep -qF 'git log -20 --pretty=format:"- %s"' "$yml" 2>/dev/null \
            && ! grep -qF 'git log --pretty=format:"- %s" "${PREV_TAG}..HEAD" | head -50' "$yml" 2>/dev/null; then
            echo '  "release_notes_git_log_pipe_safe": true'
        else
            echo '  "release_notes_git_log_pipe_safe": false'
        fi
        echo ','
        if awk '
            /^  release:/ { in_release = 1 }
            in_release && /^  smoke-test:/ { in_release = 0 }
            in_release && /actions\/checkout@v4/ { saw_checkout = 1 }
            saw_checkout && /fetch-depth: 0/ { found = 1 }
            END { exit(found ? 0 : 1) }
        ' "$yml"; then
            echo '  "release_notes_checkout_full_history": true'
        else
            echo '  "release_notes_checkout_full_history": false'
        fi
        echo ','
        local doctor_count
        doctor_count="$(grep -cE 'ee doctor --json' "$yml" 2>/dev/null || true)"
        if [ "${doctor_count:-0}" -ge 2 ]; then
            echo '  "smoke_runs_doctor": true'
        else
            echo '  "smoke_runs_doctor": false'
        fi
        echo ','
        if grep -qE 'gh release view "\$TAG"' "$yml" 2>/dev/null \
            && grep -qE 'gh release upload "\$TAG" release/\* --clobber' "$yml" 2>/dev/null; then
            echo '  "release_publish_idempotent": true'
        else
            echo '  "release_publish_idempotent": false'
        fi
        echo ','
        if grep -qF 'needs: [check-version, tag, release, smoke-test, smoke-test-macos]' "$yml" 2>/dev/null; then
            echo '  "homebrew_after_smoke": true'
        else
            echo '  "homebrew_after_smoke": false'
        fi
        echo ','
        if grep -qF 'Homebrew tap token missing' "$yml" 2>/dev/null \
            && grep -qF 'Skipping Homebrew tap PR because HOMEBREW_TAP_TOKEN is not configured.' "$yml" 2>/dev/null; then
            echo '  "homebrew_missing_token_skip_present": true'
        else
            echo '  "homebrew_missing_token_skip_present": false'
        fi
        echo ','
        if grep -qF 'git ls-remote --exit-code --heads origin "$BRANCH"' "$yml" 2>/dev/null \
            && grep -qF 'git diff --cached --quiet' "$yml" 2>/dev/null \
            && grep -qF 'gh pr list --head "$BRANCH" --base main --state open' "$yml" 2>/dev/null; then
            echo '  "homebrew_pr_idempotent": true'
        else
            echo '  "homebrew_pr_idempotent": false'
        fi
        echo ','
        if grep -qE '^concurrency:' "$yml" 2>/dev/null \
            && grep -qF 'group: release-${{ github.ref }}' "$yml" 2>/dev/null \
            && grep -qF 'cancel-in-progress: false' "$yml" 2>/dev/null; then
            echo '  "release_concurrency_present": true'
        else
            echo '  "release_concurrency_present": false'
        fi
        echo ','
        local timeout_count
        timeout_count="$(grep -cE '^[[:space:]]+timeout-minutes:' "$yml" 2>/dev/null || true)"
        if [ "${timeout_count:-0}" -ge 8 ]; then
            echo '  "job_timeouts_present": true'
        else
            echo '  "job_timeouts_present": false'
        fi
        echo ','
        local strict_mode_count
        strict_mode_count="$(grep -cF 'set -euo pipefail' "$yml" 2>/dev/null || true)"
        if [ "${strict_mode_count:-0}" -ge 14 ]; then
            echo '  "critical_bash_steps_strict": true'
        else
            echo '  "critical_bash_steps_strict": false'
        fi
        echo ','
        if grep -qF 'Missing release artifacts' "$yml" 2>/dev/null \
            && grep -qF 'find artifacts -maxdepth 1 -type f -exec mv {} release/ \;' "$yml" 2>/dev/null \
            && ! grep -qF 'mv artifacts/* release/ 2>/dev/null || true' "$yml" 2>/dev/null; then
            echo '  "release_requires_downloaded_artifacts": true'
        else
            echo '  "release_requires_downloaded_artifacts": false'
        fi
        echo ','
        local release_version_assert_count
        release_version_assert_count="$(grep -cF 'ee --version | grep -F "${{ needs.check-version.outputs.version }}"' "$yml" 2>/dev/null || true)"
        if [ "${release_version_assert_count:-0}" -ge 2 ]; then
            echo '  "smoke_version_assertions_present": true'
        else
            echo '  "smoke_version_assertions_present": false'
        fi
        echo ','
        local smoke_needs_check_version_count
        smoke_needs_check_version_count="$(grep -cF 'needs: [check-version, tag, release]' "$yml" 2>/dev/null || true)"
        if [ "${smoke_needs_check_version_count:-0}" -ge 2 ]; then
            echo '  "smoke_version_context_available": true'
        else
            echo '  "smoke_version_context_available": false'
        fi
        echo '}'
    }
}

readme_installation_status() {
    local readme="$REPO_ROOT/README.md"
    if [ ! -f "$readme" ]; then
        echo '{"probe_status":"missing","planned_markers_present":false}'
        return
    fi

    local has_status release_planned homebrew_planned cargo_planned source_available release_assets release_version_pinned
    has_status=false
    release_planned=false
    homebrew_planned=false
    cargo_planned=false
    source_available=false
    release_assets=false
    release_version_pinned=false

    if grep -qE '^### Installation status$' "$readme" 2>/dev/null; then
        has_status=true
    fi
    if grep -qF '| GitHub release installer | planned; no release assets published yet | `bd-3usjw.9` |' "$readme" 2>/dev/null; then
        release_planned=true
    fi
    if grep -qF '| Homebrew tap | planned; tap formula not published yet | `bd-3usjw.13` |' "$readme" 2>/dev/null; then
        homebrew_planned=true
    fi
    if grep -qF '| crates.io | planned; package name selected as `eidetic-engine`; binary remains `ee` | `bd-3usjw.10` |' "$readme" 2>/dev/null; then
        cargo_planned=true
    fi
    if grep -qF '| Source build | available now | this README |' "$readme" 2>/dev/null; then
        source_available=true
    fi
    if grep -qF 'releases/download/v0.1.0/install.sh' "$readme" 2>/dev/null \
        && grep -qF 'releases/download/v0.1.0/install.ps1' "$readme" 2>/dev/null; then
        release_assets=true
    fi
    if grep -qF 'EE_VERSION=v0.1.0 sh' "$readme" 2>/dev/null \
        && grep -qF -- '-Version "0.1.0"' "$readme" 2>/dev/null; then
        release_version_pinned=true
    fi

    jq -n \
        --argjson has_status "$has_status" \
        --argjson release_planned "$release_planned" \
        --argjson homebrew_planned "$homebrew_planned" \
        --argjson cargo_planned "$cargo_planned" \
        --argjson source_available "$source_available" \
        --argjson release_assets "$release_assets" \
        --argjson release_version_pinned "$release_version_pinned" \
        '{
            probe_status: "ok",
            installation_status_section: $has_status,
            release_installer_marked_planned: $release_planned,
            homebrew_marked_planned: $homebrew_planned,
            cargo_marked_planned: $cargo_planned,
            source_build_marked_available: $source_available,
            release_installer_uses_release_assets: $release_assets,
            release_installer_pins_version: $release_version_pinned,
            planned_markers_present: (
                $has_status
                and $release_planned
                and $homebrew_planned
                and $cargo_planned
                and $source_available
                and $release_assets
                and $release_version_pinned
            )
        }'
}

installer_asset_contract() {
    local unix_installer="$REPO_ROOT/install.sh"
    local windows_installer="$REPO_ROOT/install.ps1"

    local homebrew_template="$REPO_ROOT/scripts/homebrew/ee.rb.template"
    local homebrew_update="$REPO_ROOT/scripts/homebrew/update-formula.sh"

    local installer_examples_use_release_assets unix_musl unix_version_normalized unix_sigstore_hard_fail unix_sigstore_bundle_required unix_sha256_tool_required unix_sigstore_identity_bound windows_x64 windows_i686_rejected windows_arm64_rejected windows_version_normalized windows_sigstore_hard_fail windows_sigstore_bundle_required windows_sigstore_identity_bound homebrew_formula_tests_doctor homebrew_formula_fetches_sha_strictly homebrew_formula_normalizes_version_tag
    installer_examples_use_release_assets=false
    unix_musl=false
    unix_version_normalized=false
    unix_sigstore_hard_fail=false
    unix_sigstore_bundle_required=false
    unix_sha256_tool_required=false
    unix_sigstore_identity_bound=false
    windows_x64=false
    windows_i686_rejected=false
    windows_arm64_rejected=false
    windows_version_normalized=false
    windows_sigstore_hard_fail=false
    windows_sigstore_bundle_required=false
    windows_sigstore_identity_bound=false
    homebrew_formula_tests_doctor=false
    homebrew_formula_fetches_sha_strictly=false
    homebrew_formula_normalizes_version_tag=false

    if [ -f "$unix_installer" ] && grep -qF 'unknown-linux-musl' "$unix_installer" 2>/dev/null; then
        unix_musl=true
    fi
    if [ -f "$unix_installer" ] && [ -f "$windows_installer" ] \
        && grep -qF 'releases/download/v0.1.0/install.sh | EE_VERSION=v0.1.0 sh' "$unix_installer" 2>/dev/null \
        && grep -qF 'releases/download/v0.1.0/install.ps1' "$windows_installer" 2>/dev/null \
        && grep -qF -- '-Version "0.1.0"' "$windows_installer" 2>/dev/null; then
        installer_examples_use_release_assets=true
    fi
    if [ -f "$unix_installer" ] \
        && grep -qF 'VERSION="v${VERSION}"' "$unix_installer" 2>/dev/null; then
        unix_version_normalized=true
    fi
    if [ -f "$unix_installer" ] \
        && grep -qF 'error "Sigstore verification failed."' "$unix_installer" 2>/dev/null \
        && ! grep -qF 'Sigstore verification failed. Proceeding anyway.' "$unix_installer" 2>/dev/null; then
        unix_sigstore_hard_fail=true
    fi
    if [ -f "$unix_installer" ] \
        && grep -qF 'error "Failed to download Sigstore bundle"' "$unix_installer" 2>/dev/null \
        && ! grep -qF 'http_download "${SIGSTORE_URL}" "${TMPDIR}/sigstore.json" 2>/dev/null; then' "$unix_installer" 2>/dev/null; then
        unix_sigstore_bundle_required=true
    fi
    if [ -f "$unix_installer" ] \
        && grep -qF 'No SHA256 tool found. Install sha256sum or shasum, or set EE_SKIP_VERIFY=1 to bypass verification.' "$unix_installer" 2>/dev/null \
        && ! grep -qF 'No SHA256 tool found. Skipping verification.' "$unix_installer" 2>/dev/null; then
        unix_sha256_tool_required=true
    fi
    if [ -f "$unix_installer" ] \
        && grep -qF -- '--certificate-identity-regexp "$CERT_IDENTITY_REGEXP"' "$unix_installer" 2>/dev/null \
        && grep -qF -- '--certificate-oidc-issuer "$CERT_OIDC_ISSUER"' "$unix_installer" 2>/dev/null \
        && grep -qF 'https://token.actions.githubusercontent.com' "$unix_installer" 2>/dev/null; then
        unix_sigstore_identity_bound=true
    fi
    if [ -f "$windows_installer" ] && grep -qF '"AMD64" { return "x86_64" }' "$windows_installer" 2>/dev/null; then
        windows_x64=true
    fi
    if [ -f "$windows_installer" ] \
        && grep -qF '32-bit Windows is not in the release asset matrix' "$windows_installer" 2>/dev/null \
        && ! grep -qF '"x86"   { return "i686" }' "$windows_installer" 2>/dev/null; then
        windows_i686_rejected=true
    fi
    if [ -f "$windows_installer" ] \
        && grep -qF 'Windows ARM64 is not in the release asset matrix' "$windows_installer" 2>/dev/null \
        && ! grep -qF '"ARM64" { return "aarch64" }' "$windows_installer" 2>/dev/null; then
        windows_arm64_rejected=true
    fi
    if [ -f "$windows_installer" ] \
        && grep -qF '$Version.StartsWith("v")' "$windows_installer" 2>/dev/null \
        && grep -qF '$tag = $Version' "$windows_installer" 2>/dev/null \
        && grep -qF '$tag = "v$Version"' "$windows_installer" 2>/dev/null; then
        windows_version_normalized=true
    fi
    if [ -f "$windows_installer" ] \
        && grep -qF 'Write-Error-Exit "Sigstore verification failed: $result"' "$windows_installer" 2>/dev/null \
        && grep -qF 'Write-Error-Exit "Sigstore verification error: $_"' "$windows_installer" 2>/dev/null; then
        windows_sigstore_hard_fail=true
    fi
    if [ -f "$windows_installer" ] \
        && grep -qF 'if ($cosign) {' "$windows_installer" 2>/dev/null \
        && grep -qF 'Download-File (Get-ReleaseAssetUrl $Version $sigstoreName) $sigstorePath' "$windows_installer" 2>/dev/null \
        && ! grep -qF 'Sigstore bundle not available for this release.' "$windows_installer" 2>/dev/null; then
        windows_sigstore_bundle_required=true
    fi
    if [ -f "$windows_installer" ] \
        && grep -qF -- '--certificate-identity-regexp $certIdentityRegexp' "$windows_installer" 2>/dev/null \
        && grep -qF -- '--certificate-oidc-issuer $certOidcIssuer' "$windows_installer" 2>/dev/null \
        && grep -qF 'https://token.actions.githubusercontent.com' "$windows_installer" 2>/dev/null; then
        windows_sigstore_identity_bound=true
    fi
    if [ -f "$homebrew_template" ] \
        && grep -qF 'system "#{bin}/ee", "doctor", "--json"' "$homebrew_template" 2>/dev/null; then
        homebrew_formula_tests_doctor=true
    fi
    if [ -f "$homebrew_update" ] \
        && grep -qF 'curl -fsSL "$url"' "$homebrew_update" 2>/dev/null \
        && grep -qF "grep -Eq '^[[:xdigit:]]{64}$'" "$homebrew_update" 2>/dev/null \
        && grep -qF 'Invalid SHA256 digest' "$homebrew_update" 2>/dev/null; then
        homebrew_formula_fetches_sha_strictly=true
    fi
    if [ -f "$homebrew_update" ] \
        && grep -qF 'VERSION="${VERSION#v}"' "$homebrew_update" 2>/dev/null \
        && grep -qF 'RELEASE_URL="https://github.com/$REPO/releases/download/v$VERSION"' "$homebrew_update" 2>/dev/null; then
        homebrew_formula_normalizes_version_tag=true
    fi

    jq -n \
        --argjson installer_examples_use_release_assets "$installer_examples_use_release_assets" \
        --argjson unix_musl "$unix_musl" \
        --argjson unix_version_normalized "$unix_version_normalized" \
        --argjson unix_sigstore_hard_fail "$unix_sigstore_hard_fail" \
        --argjson unix_sigstore_bundle_required "$unix_sigstore_bundle_required" \
        --argjson unix_sha256_tool_required "$unix_sha256_tool_required" \
        --argjson unix_sigstore_identity_bound "$unix_sigstore_identity_bound" \
        --argjson windows_x64 "$windows_x64" \
        --argjson windows_i686_rejected "$windows_i686_rejected" \
        --argjson windows_arm64_rejected "$windows_arm64_rejected" \
        --argjson windows_version_normalized "$windows_version_normalized" \
        --argjson windows_sigstore_hard_fail "$windows_sigstore_hard_fail" \
        --argjson windows_sigstore_bundle_required "$windows_sigstore_bundle_required" \
        --argjson windows_sigstore_identity_bound "$windows_sigstore_identity_bound" \
        --argjson homebrew_formula_tests_doctor "$homebrew_formula_tests_doctor" \
        --argjson homebrew_formula_fetches_sha_strictly "$homebrew_formula_fetches_sha_strictly" \
        --argjson homebrew_formula_normalizes_version_tag "$homebrew_formula_normalizes_version_tag" \
        '{
            probe_status: "ok",
            installer_examples_use_release_assets: $installer_examples_use_release_assets,
            unix_installer_supports_musl_asset: $unix_musl,
            unix_installer_normalizes_version_tag: $unix_version_normalized,
            unix_installer_fails_on_bad_sigstore: $unix_sigstore_hard_fail,
            unix_installer_requires_sigstore_bundle_with_cosign: $unix_sigstore_bundle_required,
            unix_installer_requires_sha256_tool: $unix_sha256_tool_required,
            unix_installer_binds_sigstore_identity: $unix_sigstore_identity_bound,
            windows_installer_supports_x64_asset: $windows_x64,
            windows_installer_rejects_unbuilt_i686: $windows_i686_rejected,
            windows_installer_rejects_unbuilt_arm64: $windows_arm64_rejected,
            windows_installer_normalizes_version_tag: $windows_version_normalized,
            windows_installer_fails_on_bad_sigstore: $windows_sigstore_hard_fail,
            windows_installer_requires_sigstore_bundle_with_cosign: $windows_sigstore_bundle_required,
            windows_installer_binds_sigstore_identity: $windows_sigstore_identity_bound,
            homebrew_formula_tests_doctor_json: $homebrew_formula_tests_doctor,
            homebrew_formula_fetches_sha_strictly: $homebrew_formula_fetches_sha_strictly,
            homebrew_formula_normalizes_version_tag: $homebrew_formula_normalizes_version_tag,
            installer_targets_match_release_matrix: (
                $unix_musl
                and $installer_examples_use_release_assets
                and $unix_version_normalized
                and $unix_sigstore_hard_fail
                and $unix_sigstore_bundle_required
                and $unix_sha256_tool_required
                and $unix_sigstore_identity_bound
                and $windows_x64
                and $windows_i686_rejected
                and $windows_arm64_rejected
                and $windows_version_normalized
                and $windows_sigstore_hard_fail
                and $windows_sigstore_bundle_required
                and $windows_sigstore_identity_bound
                and $homebrew_formula_tests_doctor
                and $homebrew_formula_fetches_sha_strictly
                and $homebrew_formula_normalizes_version_tag
            )
        }'
}

ci_workflow_inventory() {
    local ci="$REPO_ROOT/.github/workflows/ci.yml"
    if [ ! -f "$ci" ]; then
        echo '{"probe_status":"missing","install_pipeline_smoke_present":false}'
        return
    fi

    local schedule_present manual_present verify_not_scheduled job_present linux_present macos_present path_gate_present docker_smoke_present skip_present artifact_upload_present smoke_uses_latest_tag smoke_uses_release_installer_asset homebrew_smoke_present cargo_smoke_present smoke_version_assertions_present
    schedule_present=false
    manual_present=false
    verify_not_scheduled=false
    job_present=false
    linux_present=false
    macos_present=false
    path_gate_present=false
    docker_smoke_present=false
    skip_present=false
    artifact_upload_present=false
    smoke_uses_latest_tag=false
    smoke_uses_release_installer_asset=false
    homebrew_smoke_present=false
    cargo_smoke_present=false
    smoke_version_assertions_present=false

    if grep -qE '^[[:space:]]*schedule:' "$ci" 2>/dev/null; then
        schedule_present=true
    fi
    if grep -qE '^[[:space:]]*workflow_dispatch:' "$ci" 2>/dev/null; then
        manual_present=true
    fi
    if grep -qF "if: github.event_name != 'schedule'" "$ci" 2>/dev/null; then
        verify_not_scheduled=true
    fi
    if grep -qE '^[[:space:]]*install-pipeline-smoke:' "$ci" 2>/dev/null; then
        job_present=true
    fi
    if grep -qF 'ubuntu-latest' "$ci" 2>/dev/null; then
        linux_present=true
    fi
    if grep -qF 'macos-14' "$ci" 2>/dev/null; then
        macos_present=true
    fi
    if grep -qF "steps.audit.outputs.decided_path == 'path_a_post_release'" "$ci" 2>/dev/null; then
        path_gate_present=true
    fi
    if grep -qF 'ubuntu:24.04 debian:bookworm-slim' "$ci" 2>/dev/null; then
        docker_smoke_present=true
    fi
    if grep -qF 'Skipping live installer smoke until GitHub release' "$ci" 2>/dev/null; then
        skip_present=true
    fi
    if grep -qF 'name: install-pipeline-audit-${{ runner.os }}' "$ci" 2>/dev/null \
        && grep -qF 'path: tests/audit_artifacts/install_pipeline_*.json' "$ci" 2>/dev/null; then
        artifact_upload_present=true
    fi
    if grep -qF 'latest_tag=$LATEST_TAG' "$ci" 2>/dev/null \
        && grep -qF 'EE_VERSION="${EE_RELEASE_TAG}"' "$ci" 2>/dev/null \
        && grep -qF 'EE_VERSION="${{ steps.audit.outputs.latest_tag }}"' "$ci" 2>/dev/null; then
        smoke_uses_latest_tag=true
    fi
    if grep -qF 'releases/download/${EE_RELEASE_TAG}/install.sh' "$ci" 2>/dev/null \
        && grep -qF 'releases/download/${{ steps.audit.outputs.latest_tag }}/install.sh' "$ci" 2>/dev/null; then
        smoke_uses_release_installer_asset=true
    fi
    if grep -qF 'brew install Dicklesworthstone/tap/ee' "$ci" 2>/dev/null; then
        homebrew_smoke_present=true
    fi
    if grep -qF 'export CARGO_HOME="$RUNNER_TEMP/clean-cargo-home"' "$ci" 2>/dev/null \
        && grep -qF 'cargo install eidetic-engine' "$ci" 2>/dev/null; then
        cargo_smoke_present=true
    fi
    if grep -qF 'EE_EXPECTED_VERSION="${EE_RELEASE_TAG#v}"' "$ci" 2>/dev/null \
        && [ "$(grep -cF 'EE_EXPECTED_VERSION="${EE_EXPECTED_VERSION#v}"' "$ci" 2>/dev/null || true)" -ge 3 ] \
        && [ "$(grep -cF 'ee --version | grep -F "$EE_EXPECTED_VERSION"' "$ci" 2>/dev/null || true)" -ge 4 ]; then
        smoke_version_assertions_present=true
    fi

    jq -n \
        --argjson schedule_present "$schedule_present" \
        --argjson manual_present "$manual_present" \
        --argjson verify_not_scheduled "$verify_not_scheduled" \
        --argjson job_present "$job_present" \
        --argjson linux_present "$linux_present" \
        --argjson macos_present "$macos_present" \
        --argjson path_gate_present "$path_gate_present" \
        --argjson docker_smoke_present "$docker_smoke_present" \
        --argjson skip_present "$skip_present" \
        --argjson artifact_upload_present "$artifact_upload_present" \
        --argjson smoke_uses_latest_tag "$smoke_uses_latest_tag" \
        --argjson smoke_uses_release_installer_asset "$smoke_uses_release_installer_asset" \
        --argjson homebrew_smoke_present "$homebrew_smoke_present" \
        --argjson cargo_smoke_present "$cargo_smoke_present" \
        --argjson smoke_version_assertions_present "$smoke_version_assertions_present" \
        '{
            probe_status: "ok",
            schedule_present: $schedule_present,
            workflow_dispatch_present: $manual_present,
            verify_not_scheduled: $verify_not_scheduled,
            install_pipeline_smoke_present: $job_present,
            linux_runner_present: $linux_present,
            macos_runner_present: $macos_present,
            path_a_gate_present: $path_gate_present,
            linux_docker_smoke_present: $docker_smoke_present,
            pre_release_skip_present: $skip_present,
            audit_artifact_upload_present: $artifact_upload_present,
            smoke_uses_latest_release_tag: $smoke_uses_latest_tag,
            smoke_uses_release_installer_asset: $smoke_uses_release_installer_asset,
            homebrew_smoke_present: $homebrew_smoke_present,
            cargo_smoke_present: $cargo_smoke_present,
            smoke_version_assertions_present: $smoke_version_assertions_present,
            weekly_smoke_ready: (
                $schedule_present
                and $manual_present
                and $verify_not_scheduled
                and $job_present
                and $linux_present
                and $macos_present
                and $path_gate_present
                and $docker_smoke_present
                and $skip_present
                and $artifact_upload_present
                and $smoke_uses_latest_tag
                and $smoke_uses_release_installer_asset
                and $homebrew_smoke_present
                and $cargo_smoke_present
                and $smoke_version_assertions_present
            )
        }'
}

# --- Determine PATH-A vs PATH-B -----------------------------------
# PATH-A (post-release): at least one published GitHub release exists
#   for the upstream repo; crates.io claims the `ee` name; tap formula
#   exists.
# PATH-B (pre-release): any of those is missing.
GH_RAW="$(gh_releases)"
CRATES_RAW="$(crates_io_inventory)"
TAP_RAW="$(brew_tap_inventory)"
DEPENDENCY_RESOLUTION_JSON="$(dependency_resolution_inventory)"
RELEASE_YML_JSON="$(release_workflow_assets)"
README_JSON="$(readme_installation_status)"
INSTALLER_JSON="$(installer_asset_contract)"
CI_JSON="$(ci_workflow_inventory)"

# Encode each raw probe as a JSON sub-field.
gh_status() {
    case "$GH_RAW" in
        'gh_cli_missing') echo '{"probe_status":"gh_cli_missing","releases":null}' ;;
        'gh_probe_failed') echo '{"probe_status":"probe_failed","releases":null}' ;;
        '[]') echo '{"probe_status":"no_releases","releases":[]}' ;;
        *) printf '{"probe_status":"ok","releases":%s}' "$GH_RAW" ;;
    esac
}

crates_status() {
    case "$CRATES_RAW" in
        'curl_missing') echo '{"probe_status":"curl_missing","name_claimed":null}' ;;
        'crates_io_unreachable') echo '{"probe_status":"unreachable","name_claimed":null}' ;;
        *)
            # Crates.io 404 returns a JSON error body; success returns
            # `{"crate":{...},"versions":[...],...}`.
            if printf '%s' "$CRATES_RAW" | jq -e '.crate' >/dev/null 2>&1; then
                local latest_version repository homepage documentation created_at updated_at owners_json
                latest_version="$(printf '%s' "$CRATES_RAW" | jq -r '.crate.newest_version // "unknown"')"
                repository="$(printf '%s' "$CRATES_RAW" | jq -r '.crate.repository // ""')"
                homepage="$(printf '%s' "$CRATES_RAW" | jq -r '.crate.homepage // ""')"
                documentation="$(printf '%s' "$CRATES_RAW" | jq -r '.crate.documentation // ""')"
                created_at="$(printf '%s' "$CRATES_RAW" | jq -r '.crate.created_at // ""')"
                updated_at="$(printf '%s' "$CRATES_RAW" | jq -r '.crate.updated_at // ""')"
                owners_json='null'
                local owners_response
                if owners_response="$(curl -fsSL --max-time 10 \
                    "https://crates.io/api/v1/crates/${CRATES_IO_PACKAGE_NAME}/owners" 2>/dev/null)" \
                    && printf '%s' "$owners_response" | jq -e '.users' >/dev/null 2>&1; then
                    owners_json="$(printf '%s' "$owners_response" \
                        | jq -c '[.users[]? | {id, login, name}]')"
                fi
                jq -n \
                    --arg version "$latest_version" \
                    --arg repository "$repository" \
                    --arg homepage "$homepage" \
                    --arg documentation "$documentation" \
                    --arg created_at "$created_at" \
                    --arg updated_at "$updated_at" \
                    --arg crate_name "$CRATES_IO_PACKAGE_NAME" \
                    --argjson owners "$owners_json" \
                    '{
                        probe_status: "ok",
                        crate_name: $crate_name,
                        name_claimed: true,
                        latest_version: $version,
                        repository: $repository,
                        homepage: $homepage,
                        documentation: $documentation,
                        created_at: $created_at,
                        updated_at: $updated_at,
                        owners: $owners
                    }'
            else
                jq -n --arg crate_name "$CRATES_IO_PACKAGE_NAME" \
                    '{probe_status: "ok", crate_name: $crate_name, name_claimed: false, error_body: input | tojson}' <<<"$CRATES_RAW"
            fi
            ;;
    esac
}

tap_status() {
    case "$TAP_RAW" in
        'curl_missing') echo '{"probe_status":"curl_missing","formula_present":null}' ;;
        'tap_unreachable') echo '{"probe_status":"unreachable","formula_present":null}' ;;
        *)
            if [ -z "$TAP_RAW" ]; then
                echo '{"probe_status":"ok","formula_present":false}'
            else
                # Crude version pin extraction: `url ".../v0.X.Y.tar.gz"`.
                local pinned_version
                pinned_version="$(printf '%s' "$TAP_RAW" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1)"
                jq -n --arg version "${pinned_version:-unknown}" \
                    '{probe_status: "ok", formula_present: true, version_pin: $version}'
            fi
            ;;
    esac
}

release_assets_status() {
    local tag
    tag="$(printf '%s' "$GH_JSON" | jq -r '(.releases // [])[0].tagName // ""' 2>/dev/null)"
    if [ -z "$tag" ]; then
        jq -n '{probe_status: "no_release", tag: "", expected_assets: [], asset_names: [], missing_assets: [], asset_matrix_complete: false}'
        return
    fi
    if ! command -v gh >/dev/null 2>&1; then
        jq -n --arg tag "$tag" '{probe_status: "gh_cli_missing", tag: $tag, expected_assets: [], asset_names: [], missing_assets: [], asset_matrix_complete: false}'
        return
    fi

    local view_json
    if ! view_json="$(gh -R Dicklesworthstone/eidetic_engine_cli release view "$tag" --json assets 2>/dev/null)"; then
        jq -n --arg tag "$tag" '{probe_status: "probe_failed", tag: $tag, expected_assets: [], asset_names: [], missing_assets: [], asset_matrix_complete: false}'
        return
    fi

    local targets_csv asset_names expected_assets
    targets_csv="$(printf '%s' "$RELEASE_YML_JSON" | jq -r '.targets_csv // ""' 2>/dev/null)"
    asset_names="$(printf '%s' "$view_json" | jq -c '[.assets[]?.name]')"
    expected_assets="$(jq -n --arg targets_csv "$targets_csv" '
        ($targets_csv | split(",") | map(select(length > 0))) as $targets
        | [
            $targets[] as $target
            | "ee-\($target).tar.xz",
              "ee-\($target).tar.xz.sha256",
              "ee-\($target).tar.xz.sigstore.json"
          ]
          + ["install.sh", "install.ps1"]
    ')"

    jq -n \
        --arg tag "$tag" \
        --argjson expected "$expected_assets" \
        --argjson names "$asset_names" \
        '{
            probe_status: "ok",
            tag: $tag,
            expected_assets: $expected,
            asset_names: $names,
            missing_assets: ($expected - $names),
            asset_matrix_complete: (($expected - $names) | length == 0)
        }'
}

GH_JSON="$(gh_status)"
CRATES_JSON="$(crates_status)"
TAP_JSON="$(tap_status)"
RELEASE_ASSETS_JSON="$(release_assets_status)"

# Decide path. PATH-A requires all install surfaces to point at the
# same current release. A claimed-but-foreign crate, stale tap
# formula, or crates.io version mismatch is still PATH-B.
DECIDE_PATH="path_b_pre_release"
DECIDE_REASON_JSON="$(jq -n \
    --argjson gh "$GH_JSON" \
    --argjson crates "$CRATES_JSON" \
    --argjson tap "$TAP_JSON" \
    --argjson dependency_resolution "$DEPENDENCY_RESOLUTION_JSON" \
    --argjson release_assets "$RELEASE_ASSETS_JSON" \
    --arg project_repo "https://github.com/Dicklesworthstone/eidetic_engine_cli" \
    --arg crate_name "$CRATES_IO_PACKAGE_NAME" \
    '{
        crates_package_name: $crate_name,
        dep_resolution_ready: ($dependency_resolution.dep_resolution_ready // false),
        unresolved_dependency_count: ($dependency_resolution.unresolved_count // null),
        release_tag: (($gh.releases // [])[0].tagName // ""),
        release_version: ((($gh.releases // [])[0].tagName // "") | sub("^v"; "")),
        gh_releases: ($gh.releases // null) | (. != null and (length > 0)),
        latest_release_published: (
            (($gh.releases // [])[0].tagName // "") != ""
            and (($gh.releases // [])[0].publishedAt // "") != ""
            and ((($gh.releases // [])[0].isDraft // false) == false)
            and ((($gh.releases // [])[0].isPrerelease // false) == false)
        ),
        release_assets_complete: ($release_assets.asset_matrix_complete // false),
        crates_name_claimed: ($crates.name_claimed // false),
        crates_points_at_project: (($crates.repository // "") == $project_repo),
        crates_version_matches_release: (
            ($crates.latest_version // "") != ""
            and ($crates.latest_version // "") == (((($gh.releases // [])[0].tagName // "") | sub("^v"; "")))
        ),
        tap_formula_present: ($tap.formula_present // false),
        tap_version_matches_release: (
            ($tap.version_pin // "") != ""
            and (($tap.version_pin // "") | sub("^v"; "")) == (((($gh.releases // [])[0].tagName // "") | sub("^v"; "")))
        )
    }')"

if [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.gh_releases')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.latest_release_published')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.release_assets_complete')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.dep_resolution_ready')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.crates_name_claimed')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.crates_points_at_project')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.crates_version_matches_release')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.tap_formula_present')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.tap_version_matches_release')" = "true" ]; then
    DECIDE_PATH="path_a_post_release"
fi

# Build the envelope.
jq -n \
    --arg schema "ee.audit.install_pipeline.v1" \
    --arg generated_at "$ISO_TIMESTAMP" \
    --arg src_commit "$SRC_COMMIT" \
    --arg decided_path "$DECIDE_PATH" \
    --argjson decision_inputs "$DECIDE_REASON_JSON" \
    --argjson gh "$GH_JSON" \
    --argjson crates "$CRATES_JSON" \
    --argjson tap "$TAP_JSON" \
    --argjson release_assets "$RELEASE_ASSETS_JSON" \
    --argjson dependency_resolution "$DEPENDENCY_RESOLUTION_JSON" \
    --argjson release_workflow "$RELEASE_YML_JSON" \
    --argjson readme_installation "$README_JSON" \
    --argjson installer_assets "$INSTALLER_JSON" \
    --argjson ci_workflow "$CI_JSON" \
    '{
        schema: $schema,
        generated_at: $generated_at,
        src_commit: $src_commit,
        decided_path: $decided_path,
        decision_inputs: $decision_inputs,
        github_releases: $gh,
        github_release_assets: $release_assets,
        crates_io: $crates,
        dependency_resolution: $dependency_resolution,
        homebrew_tap: $tap,
        release_workflow: $release_workflow,
        readme_installation: $readme_installation,
        installer_assets: $installer_assets,
        ci_workflow: $ci_workflow,
        next_actions: (
            if $decided_path == "path_a_post_release" then
                ["Run install.sh on clean Docker images (ubuntu:24.04, debian:bookworm-slim)",
                 "Run brew install Dicklesworthstone/tap/ee on macOS",
                 "Run cargo install eidetic-engine in a clean ~/.cargo",
                 "Verify each artifact with cosign verify-blob against the sigstore bundle"]
            else
                ["Track bd-3usjw.11: Resolve dependency_resolution.required_crates before cargo publish",
                 "Track bd-3usjw.10: Publish eidetic-engine package with bin name ee",
                 "Track bd-3usjw.13: Publish Dicklesworthstone/homebrew-tap with Formula/ee.rb pinned to v0.1.0",
                 "Track bd-3usjw.9: Ship v0.1.0 GitHub release with full asset set (ee-{target}.tar.xz + .sha256 + sigstore.json + install.sh + install.ps1)",
                 "Keep release-readiness followups under bd-3usjw with the release-readiness label"]
            end
        )
    }' > "$OUTPUT"

# Maintain a stable latest-symlink for CI to read.
ln -sf "$(basename "$OUTPUT")" "$LATEST_SYMLINK"

echo "wrote $OUTPUT"
echo "decided_path: $DECIDE_PATH"
jq -r '.next_actions[]' "$OUTPUT" | sed 's/^/  - /'
