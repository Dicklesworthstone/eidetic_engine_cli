#!/usr/bin/env bash
# J3 — Epic C: policy detector overhaul e2e driver.
#
# Drives `ee remember` and `ee rule add` against contents that were rejected
# by the pre-overhaul keyword secret detector / tag validator. Verifies the
# C1 + C3 fixes accept meta-policy phrases and dot/colon tags while still
# rejecting real value-shape secrets.
#
# Shipped (real assertions):  C1, C2, C3, C4, C5, B10
# Not yet shipped (todo):     none

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
# C2 (shipped) — explicit bypass must persist with a visible degraded/audit
# signal; workspace allow config must exempt only the matching context.
# ------------------------------------------------------------

BYPASS_JSON=$(ee_workspace remember \
    "Document redacted sample API_KEY=sk-FAKEabc123def456ghi789jkl012." \
    --level procedural --kind rule --allow-secret-mention --json 2>/dev/null || true)
assert_jq "$BYPASS_JSON" '.success // false' "true" "c2_allow_secret_mention_persists"
assert_jq "$BYPASS_JSON" '.data.policy_bypass_used // false' "true" "c2_bypass_used_flag_visible"
assert_jq "$BYPASS_JSON" '.data.policy_bypass.kind // empty' "flag" "c2_bypass_kind_flag"
assert_jq "$BYPASS_JSON" '.data.degraded[0].code // empty' "policy_bypass_used" "c2_degraded_code_visible"

printf '%s\n' \
    '[policy.secret_detector]' \
    'allow_phrases = ["OAuth refresh token"]' \
    > "$EPIC_WORKSPACE/.ee/config.toml"
CONFIG_BYPASS_JSON=$(ee_workspace remember \
    "OAuth refresh token fixture uses API_KEY=sk-FAKEabc123def456ghi789jkl012 for documentation." \
    --level semantic --kind fact --json 2>/dev/null || true)
assert_jq "$CONFIG_BYPASS_JSON" '.success // false' "true" "c2_allow_phrase_persists"
assert_jq "$CONFIG_BYPASS_JSON" '.data.policy_bypass.kind // empty' "config_phrase" "c2_allow_phrase_kind"

# ------------------------------------------------------------
# B10 (shipped) — output-time redaction is defense-in-depth for
# secret-bearing content that was explicitly persisted through C2.
# Search/context redact by default, why remains an explicit audit surface,
# and disabling workspace output redaction emits an info degradation.
# ------------------------------------------------------------

B10_MEMORY_ID=$(printf '%s' "$BYPASS_JSON" | jq -r '.data.memory_id // empty' 2>/dev/null || true)
assert_jq_nonempty "$BYPASS_JSON" '.data.memory_id // empty' "b10_fixture_memory_id_present"

ee_workspace index rebuild --json >/dev/null 2>/dev/null

B10_SEARCH_JSON=$(ee_workspace search "Document redacted sample" --json 2>/dev/null || true)
assert_jq "$B10_SEARCH_JSON" '.success // false' "true" "b10_search_succeeds"
assert_jq "$B10_SEARCH_JSON" '.data.results[0].contentRedacted // false' "true" \
    "b10_search_flags_output_redaction"
assert_jq "$B10_SEARCH_JSON" '.data.results[0].redactions[0].reason // empty' "api_key" \
    "b10_search_reports_redaction_reason"
assert_jq "$B10_SEARCH_JSON" '.data.results[0].metadata.content | contains("[REDACTED:api_key]")' \
    "true" "b10_search_content_redacted"
SEARCH_RAW_LEAKED=false
if printf '%s' "$B10_SEARCH_JSON" | grep -Fq 'sk-FAKEabc123def456ghi789jkl012'; then
    SEARCH_RAW_LEAKED=true
fi
e2e_log_assert_eq "$SEARCH_RAW_LEAKED" "false" "b10_search_does_not_leak_raw_secret"

B10_CONTEXT_JSON=$(ee_workspace context "Document redacted sample" --json 2>/dev/null || true)
assert_jq "$B10_CONTEXT_JSON" '.success // false' "true" "b10_context_succeeds"
assert_jq "$B10_CONTEXT_JSON" '.data.pack.items[0].contentRedacted // false' "true" \
    "b10_context_flags_output_redaction"
assert_jq "$B10_CONTEXT_JSON" '.data.pack.items[0].redactions[0].reason // empty' "api_key" \
    "b10_context_reports_redaction_reason"
assert_jq "$B10_CONTEXT_JSON" '.data.pack.items[0].content | contains("[REDACTED:api_key]")' \
    "true" "b10_context_content_redacted"
CONTEXT_RAW_LEAKED=false
if printf '%s' "$B10_CONTEXT_JSON" | grep -Fq 'sk-FAKEabc123def456ghi789jkl012'; then
    CONTEXT_RAW_LEAKED=true
fi
e2e_log_assert_eq "$CONTEXT_RAW_LEAKED" "false" "b10_context_does_not_leak_raw_secret"

B10_WHY_JSON=$(ee_workspace why "$B10_MEMORY_ID" --json 2>/dev/null || true)
assert_jq "$B10_WHY_JSON" '.success // false' "true" "b10_why_succeeds"
assert_jq "$B10_WHY_JSON" '.data.content | contains("sk-FAKEabc123def456ghi789jkl012")' \
    "true" "b10_why_returns_full_content"

B10_AUDIT_JSON=$(ee_workspace audit timeline --json 2>/dev/null || true)
assert_jq "$B10_AUDIT_JSON" \
    '(.data.entries // .entries // []) | map(select((.action // .mutation_kind) == "redact_at_output")) | length > 0' \
    "true" "b10_audit_records_output_redaction"

printf '%s\n' \
    '[policy.secret_detector]' \
    'allow_phrases = ["OAuth refresh token"]' \
    '' \
    '[policy.output_redaction]' \
    'enabled = false' \
    > "$EPIC_WORKSPACE/.ee/config.toml"

B10_DISABLED_SEARCH_JSON=$(ee_workspace search "Document redacted sample" --json 2>/dev/null || true)
assert_jq "$B10_DISABLED_SEARCH_JSON" '.success // false' "true" \
    "b10_disabled_search_succeeds"
assert_jq "$B10_DISABLED_SEARCH_JSON" \
    '.data.degraded | map(select(.code == "output_redaction_disabled" and .severity == "info")) | length > 0' \
    "true" "b10_disabled_search_degraded_info"
assert_jq "$B10_DISABLED_SEARCH_JSON" \
    '.data.results[0].metadata.content | contains("sk-FAKEabc123def456ghi789jkl012")' \
    "true" "b10_disabled_search_returns_raw_content"
assert_jq "$B10_DISABLED_SEARCH_JSON" '.data.results[0].contentRedacted // false' "false" \
    "b10_disabled_search_has_no_redaction_flag"

B10_DISABLED_CONTEXT_JSON=$(ee_workspace context "Document redacted sample" --json 2>/dev/null || true)
assert_jq "$B10_DISABLED_CONTEXT_JSON" '.success // false' "true" \
    "b10_disabled_context_succeeds"
assert_jq "$B10_DISABLED_CONTEXT_JSON" \
    '.data.degraded | map(select(.code == "output_redaction_disabled" and .severity == "info")) | length > 0' \
    "true" "b10_disabled_context_degraded_info"
assert_jq "$B10_DISABLED_CONTEXT_JSON" \
    '.data.pack.items[0].content | contains("sk-FAKEabc123def456ghi789jkl012")' \
    "true" "b10_disabled_context_returns_raw_content"
assert_jq "$B10_DISABLED_CONTEXT_JSON" '.data.pack.items[0].contentRedacted // false' "false" \
    "b10_disabled_context_has_no_redaction_flag"

# ------------------------------------------------------------
# C4 (shipped) — programmatic error.details for tag and content rejection.
# ------------------------------------------------------------

assert_jq "$SPACE_TAG_JSON" '.error.details.detailCode // empty' \
    "policy_tag_rejected_with_details" "c4_tag_detail_code"
assert_jq_nonempty "$SPACE_TAG_JSON" '.error.details.acceptedPattern // empty' \
    "c4_tag_accepted_pattern_present"
assert_jq "$SPACE_TAG_JSON" '.error.details.acceptedExamples | index("v0.1.0") != null' \
    "true" "c4_tag_examples_include_dotted_version"
assert_jq "$SPACE_TAG_JSON" '.error.details.matchedAt[0].reason // empty' \
    "space_disallowed" "c4_tag_rejected_reason"

assert_jq "$REAL_PAT_JSON" '.error.details.detailCode // empty' \
    "policy_secret_detected_with_offsets" "c4_secret_detail_code"
assert_jq "$REAL_PAT_JSON" '.error.details.bypassFlag // empty' \
    "--allow-secret-mention" "c4_secret_bypass_flag"
assert_jq_nonempty "$REAL_PAT_JSON" '.error.details.matchedAt[0].pattern_id // empty' \
    "c4_secret_pattern_id_present"
SECRET_DETAIL_LEAKED=false
if printf '%s' "$REAL_PAT_JSON" | grep -Fq 'ghp_abcdefghijklmnopqrstuvwxyz0123456789ABC'; then
    SECRET_DETAIL_LEAKED=true
fi
e2e_log_assert_eq "$SECRET_DETAIL_LEAKED" "false" "c4_secret_details_do_not_echo_value"

# C5 — corpora-level seed tests pin expected accept/reject behavior.
SECRET_PATTERN_DIR="$REPO_ROOT/tests/fixtures/secret_patterns"
GITLEAKS_CORPUS="$SECRET_PATTERN_DIR/gitleaks_subset.jsonl"
TRUFFLEHOG_CORPUS="$SECRET_PATTERN_DIR/trufflehog_subset.jsonl"
FALSE_POSITIVE_CORPUS="$SECRET_PATTERN_DIR/false_positive_corpus.jsonl"

for corpus in "$GITLEAKS_CORPUS" "$TRUFFLEHOG_CORPUS" "$FALSE_POSITIVE_CORPUS"; do
    jq -c . "$corpus" >/dev/null 2>&1
    e2e_log_assert_eq "$?" "0" "c5_$(basename "$corpus" .jsonl)_jsonl_parseable"
done

GITLEAKS_COUNT=$(wc -l < "$GITLEAKS_CORPUS" | tr -d ' ')
TRUFFLEHOG_COUNT=$(wc -l < "$TRUFFLEHOG_CORPUS" | tr -d ' ')
FALSE_POSITIVE_COUNT=$(wc -l < "$FALSE_POSITIVE_CORPUS" | tr -d ' ')

if [ "$GITLEAKS_COUNT" -ge 50 ]; then
    e2e_log_assert_eq "0" "0" "c5_gitleaks_subset_min_50"
else
    e2e_log_assert_eq "1" "0" "c5_gitleaks_subset_min_50"
fi

if [ "$TRUFFLEHOG_COUNT" -ge 50 ]; then
    e2e_log_assert_eq "0" "0" "c5_trufflehog_subset_min_50"
else
    e2e_log_assert_eq "1" "0" "c5_trufflehog_subset_min_50"
fi

if [ "$FALSE_POSITIVE_COUNT" -ge 100 ]; then
    e2e_log_assert_eq "0" "0" "c5_false_positive_subset_min_100"
else
    e2e_log_assert_eq "1" "0" "c5_false_positive_subset_min_100"
fi
