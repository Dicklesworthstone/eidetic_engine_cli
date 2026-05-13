#!/usr/bin/env bash
# bd-2gill — install pipeline audit (Phase 1: read-only inventory).
#
# Probes the three install paths advertised in README:
#
#   - curl-pipe install.sh + Sigstore-verified bundle
#   - brew install Dicklesworthstone/tap/ee
#   - cargo install ee from crates.io
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

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
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
    if out="$(gh -R Dicklesworthstone/eidetic_engine_cli release list --limit 5 --json tagName,isLatest,isDraft,publishedAt,name 2>&1)"; then
        printf '%s' "$out"
    else
        # gh might exit non-zero on auth issues / network errors.
        echo 'gh_probe_failed'
        return
    fi
}

# --- Probe 2: crates.io for the `ee` crate ------------------------
crates_io_inventory() {
    if ! command -v curl >/dev/null 2>&1; then
        echo 'curl_missing'
        return
    fi
    local response
    if response="$(curl -fsSL --max-time 10 \
        'https://crates.io/api/v1/crates/ee' 2>&1)"; then
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

# --- Probe 4: parse release.yml for the asset matrix ---------------
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
        targets="$(grep -oE 'target:.*\[.*\]' "$yml" | head -1 \
            || grep -A6 'matrix:' "$yml" | grep -oE '[a-z0-9_]+-[a-z0-9_-]+-[a-z0-9_]+(-[a-z0-9_]+)?(-msvc)?' | sort -u | tr '\n' ',' | sed 's/,$//')"
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
        echo '}'
    }
}

# --- Determine PATH-A vs PATH-B -----------------------------------
# PATH-A (post-release): at least one published GitHub release exists
#   for the upstream repo; crates.io claims the `ee` name; tap formula
#   exists.
# PATH-B (pre-release): any of those is missing.
GH_RAW="$(gh_releases)"
CRATES_RAW="$(crates_io_inventory)"
TAP_RAW="$(brew_tap_inventory)"
RELEASE_YML_JSON="$(release_workflow_assets)"

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
                local latest_version
                latest_version="$(printf '%s' "$CRATES_RAW" | jq -r '.crate.newest_version // "unknown"')"
                jq -n --arg version "$latest_version" \
                    '{probe_status: "ok", name_claimed: true, latest_version: $version}'
            else
                jq -n '{probe_status: "ok", name_claimed: false, error_body: input | tojson}' <<<"$CRATES_RAW"
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

GH_JSON="$(gh_status)"
CRATES_JSON="$(crates_status)"
TAP_JSON="$(tap_status)"

# Decide path. PATH-A requires all three positive signals AND a
# non-empty releases array.
DECIDE_PATH="path_b_pre_release"
DECIDE_REASON_JSON="$(jq -n \
    --argjson gh "$GH_JSON" \
    --argjson crates "$CRATES_JSON" \
    --argjson tap "$TAP_JSON" \
    '{
        gh_releases: ($gh.releases // null) | (. != null and (length > 0)),
        crates_name_claimed: ($crates.name_claimed // false),
        tap_formula_present: ($tap.formula_present // false)
    }')"

if [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.gh_releases')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.crates_name_claimed')" = "true" ] \
    && [ "$(printf '%s' "$DECIDE_REASON_JSON" | jq -r '.tap_formula_present')" = "true" ]; then
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
    --argjson release_workflow "$RELEASE_YML_JSON" \
    '{
        schema: $schema,
        generated_at: $generated_at,
        src_commit: $src_commit,
        decided_path: $decided_path,
        decision_inputs: $decision_inputs,
        github_releases: $gh,
        crates_io: $crates,
        homebrew_tap: $tap,
        release_workflow: $release_workflow,
        next_actions: (
            if $decided_path == "path_a_post_release" then
                ["Run install.sh on clean Docker images (ubuntu:24.04, debian:bookworm-slim)",
                 "Run brew install Dicklesworthstone/tap/ee on macOS",
                 "Run cargo install ee in a clean ~/.cargo",
                 "Verify each artifact with cosign verify-blob against the sigstore bundle"]
            else
                ["File bd-2gill.A: Claim ee crate name on crates.io and publish initial release",
                 "File bd-2gill.B: Publish Dicklesworthstone/homebrew-tap with Formula/ee.rb pinned to v0.1.0",
                 "File bd-2gill.C: Ship v0.1.0 GitHub release with full asset set (ee-{target}.tar.xz + .sha256 + sigstore.json + install.sh + install.ps1)",
                 "Wire each followup as parent-child child of bd-2gill, label install-pipeline-followup"]
            end
        )
    }' > "$OUTPUT"

# Maintain a stable latest-symlink for CI to read.
ln -sf "$(basename "$OUTPUT")" "$LATEST_SYMLINK"

echo "wrote $OUTPUT"
echo "decided_path: $DECIDE_PATH"
jq -r '.next_actions[]' "$OUTPUT" | sed 's/^/  - /'
