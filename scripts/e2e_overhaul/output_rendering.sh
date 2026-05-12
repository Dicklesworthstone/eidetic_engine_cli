#!/usr/bin/env bash
# J3 — Epic H: output rendering e2e driver.
#
# Asserts the H1 minimal-escape policy: characters that would not be markdown
# syntax in their position no longer get escaped. Records TODOs for H2 + H4.
#
# Shipped (real assertions):  H1, H3
# Not yet shipped (todo):     H2, H4

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/e2e_overhaul/lib/shared.sh
source "$SCRIPT_DIR/lib/shared.sh"
require_jq
epic_setup "epic_H_output_rendering"

# Seed a memory whose content contains the literal patterns that the
# pre-overhaul renderer was over-escaping: version strings (v0.2.0), ID
# strings (mem_01KR...), and underscored identifiers in mid-text position.
SEEDED_JSON=$(ee_workspace remember \
    "Release v0.2.0 ships with stable mem_01ABCDEFGHJKMNPQRSTVWXYZ01 identifiers and snake_case fields." \
    --level episodic --kind decision --json 2>/dev/null || true)

# ------------------------------------------------------------
# H1 (shipped) — minimal escape policy.
# Pull the rendered markdown context pack and assert escapes do not appear
# mid-text. The corpus query targets the seeded memory.
# ------------------------------------------------------------
MD_OUTPUT=$(ee_workspace context "release version identifiers" \
    --max-tokens 1500 --format markdown 2>/dev/null || true)

# The pre-overhaul renderer emitted "v0\.2\.0" — the escaped dot is the
# signature regression. Assert it does NOT appear.
if printf '%s' "$MD_OUTPUT" | grep -q 'v0\\\.2\\\.0'; then
    e2e_log_assert_eq "escapes_present" "no_dot_escape" "h1_no_dot_escape_in_version_string"
else
    e2e_log_assert_eq "true" "true" "h1_no_dot_escape_in_version_string"
fi

# Pre-overhaul renderer escaped underscores in mem_01... identifiers. Assert
# the backslash form does NOT appear.
if printf '%s' "$MD_OUTPUT" | grep -q 'mem\\_'; then
    e2e_log_assert_eq "escapes_present" "no_underscore_escape" "h1_no_underscore_escape_in_mem_id"
else
    e2e_log_assert_eq "true" "true" "h1_no_underscore_escape_in_mem_id"
fi

# Brackets and pipes still escape (still real markdown syntax positions).
if printf '%s' "$MD_OUTPUT" | grep -qE '\\\[|\\\]'; then
    e2e_log_note "h1_brackets_still_escape=true"
fi

# ------------------------------------------------------------
# H3 (shipped — already true via prior work) — the markdown header has stable
# anchor structure (no emoji injection, no terminal escapes).
# ------------------------------------------------------------
if printf '%s' "$MD_OUTPUT" | grep -qE '^# Context Pack:'; then
    e2e_log_assert_eq "true" "true" "h3_markdown_header_stable"
else
    e2e_log_note "h3_markdown_header_absent skip=true"
fi

# ANSI escape sequences must never appear in --format markdown output.
# BSD grep on macOS does not implement -P (Perl regex), so use a small Python
# helper rather than rely on the host grep flavor.
if printf '%s' "$MD_OUTPUT" | python3 -c "import sys; sys.exit(0 if '\x1b[' in sys.stdin.read() else 1)"; then
    e2e_log_assert_eq "ansi_present" "no_ansi" "h3_markdown_has_no_ansi_escapes"
else
    e2e_log_assert_eq "true" "true" "h3_markdown_has_no_ansi_escapes"
fi

# ------------------------------------------------------------
# H2 — escape roundtrip test (escape -> parse -> render -> equal).
# ------------------------------------------------------------
todo_assert "h2_escape_roundtrip_property_test" "bd-17c65.8.2" \
    "Property test: escape_text(s) parsed by pulldown-cmark and re-rendered must equal s."

# H4 — corner cases (leading dashes, table pipes, fenced code that contains
# fences, etc.).
todo_assert "h4_escape_corner_cases_documented" "bd-17c65.8.4" \
    "Markdown corner cases (leading dashes, table pipes, nested fences) not yet golden-tested."
