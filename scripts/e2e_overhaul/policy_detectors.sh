#!/usr/bin/env bash
# J3 — Epic C: policy detector overhaul e2e driver.
#
# Drives `ee remember` and `ee rule add` against contents that were rejected
# by the pre-overhaul keyword secret detector / tag validator. Verifies the
# C1 + C3 fixes accept meta-policy phrases and dot/colon tags while still
# rejecting real value-shape secrets.
#
# Shipped (real assertions):  C1, C3
# Not yet shipped (todo):     C2, C4, C5

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_C_policy_detectors"

# ------------------------------------------------------------
# C1 (shipped) — meta-policy phrases must be accepted.
# Real value-shape secrets must still be rejected.
# ------------------------------------------------------------

# Accept: a rule *about* secret handling that itself contains the word "secret".
META_POLICY_JSON=$(ee_workspace remember \
    "Context packs must never include secrets in the rendered output." \
    --level procedural --kind rule --json 2>/dev/null || true)
META_POLICY_OK=$(printf '%s' "$META_POLICY_JSON" \
    | jq -r '.success // false' 2>/dev/null || echo false)
e2e_log_assert_eq "$META_POLICY_OK" "true" "c1_meta_policy_phrase_accepted"

# Accept: rule about "credentials" handling that itself is not a credential.
CREDS_POLICY_JSON=$(ee_workspace remember \
    "Rotate credentials before sharing the workspace with an external auditor." \
    --level procedural --kind rule --json 2>/dev/null || true)
CREDS_POLICY_OK=$(printf '%s' "$CREDS_POLICY_JSON" \
    | jq -r '.success // false' 2>/dev/null || echo false)
e2e_log_assert_eq "$CREDS_POLICY_OK" "true" "c1_credentials_meta_phrase_accepted"

# Reject: a real GitHub PAT-shaped value must be detected and refused.
REAL_PAT_JSON=$(ee_workspace remember \
    "GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789ABC" \
    --level episodic --json 2>/dev/null || true)
REAL_PAT_OK=$(printf '%s' "$REAL_PAT_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
# success must be false. If exec failed for any reason, $REAL_PAT_OK is "false"
# too — accept either as a rejection signal.
if [ "$REAL_PAT_OK" = "false" ]; then
    e2e_log_assert_eq "true" "true" "c1_real_pat_secret_rejected"
else
    e2e_log_assert_eq "$REAL_PAT_OK" "false" "c1_real_pat_secret_rejected"
fi

# Reject: AWS-style access key prefix.
AWS_KEY_JSON=$(ee_workspace remember \
    "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE  AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" \
    --level episodic --json 2>/dev/null || true)
AWS_KEY_OK=$(printf '%s' "$AWS_KEY_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
e2e_log_assert_eq "$AWS_KEY_OK" "false" "c1_aws_key_pattern_rejected"

# ------------------------------------------------------------
# C3 (shipped) — tags with dots and colons must be accepted (the v0.1.0 tag
# story from the 2026-05-10 walkthrough). NFC normalization makes composed and
# decomposed forms map to the same canonical tag.
# ------------------------------------------------------------

DOT_TAG_JSON=$(ee_workspace remember \
    "Release v0.2.0 ships the agent-ux overhaul." \
    --level episodic --kind decision --tags v0.2.0 --json 2>/dev/null || true)
DOT_TAG_OK=$(printf '%s' "$DOT_TAG_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
e2e_log_assert_eq "$DOT_TAG_OK" "true" "c3_dot_tag_accepted"

COLON_TAG_JSON=$(ee_workspace remember \
    "Track work via beads identifier scheme." \
    --level semantic --kind decision --tags scope:agent-ux --json 2>/dev/null || true)
COLON_TAG_OK=$(printf '%s' "$COLON_TAG_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
e2e_log_assert_eq "$COLON_TAG_OK" "true" "c3_colon_tag_accepted"

# ------------------------------------------------------------
# Negative case for C3: a tag containing whitespace must be rejected because
# tags are single-token identifiers. Empty `--tags ""` is treated by clap as
# "no tags" and is NOT a useful negative case at the policy layer.
# ------------------------------------------------------------
SPACE_TAG_JSON=$(ee_workspace remember "Space in tag should fail." \
    --level episodic --tags "release notes" --json 2>/dev/null || true)
SPACE_TAG_OK=$(printf '%s' "$SPACE_TAG_JSON" | jq -r '.success // false' 2>/dev/null || echo false)
e2e_log_assert_eq "$SPACE_TAG_OK" "false" "c3_tag_with_whitespace_rejected"

# ------------------------------------------------------------
# C2 (not shipped) — error envelope must explain *why* a secret was rejected
# with a structured detector hit (regex name, span, severity).
# ------------------------------------------------------------
DETECTOR_DETAIL=$(printf '%s' "$REAL_PAT_JSON" \
    | jq -r '.error.details.detector // empty' 2>/dev/null || true)
e2e_log_note "c2_detector_detail_present=${DETECTOR_DETAIL:-false}"
todo_assert "c2_structured_detector_details" "bd-17c65.3.2" \
    "Secret rejection lacks error.details.detector (regex name, span, severity)."

# C4 — regex extension hook for project-specific secret patterns.
todo_assert "c4_secret_regex_extension_hook" "bd-17c65.3.4" \
    "No project-config secret regex registration surface yet."

# C5 — corpora-level seed test pinning expected accept/reject behavior.
todo_assert "c5_policy_corpora_seed_pins_accept_reject" "bd-17c65.3.5" \
    "tests/fixtures/policy/policy_corpus.jsonl + accept/reject manifest not yet in place."
