# Conformance Test Documentation

This directory contains conformance test suites that verify `ee` behavior against
documented specifications. Each suite validates that implementation matches the
schema contracts defined in `docs/`.

---

## Query File Conformance: `ee.query.v1`

**Test File:** `conformance/query_v1_matrix.rs`  
**Schema Doc:** `docs/query-schema.md`

### Coverage Matrix

| Category | Count | Description |
|----------|-------|-------------|
| Implemented Features | 23 | Working features that should succeed |
| Unimplemented Features | 1 | Features returning `ERR_UNSUPPORTED_FEATURE` |
| Error Cases | 11 | Invalid inputs returning appropriate error codes |
| Combination Tests | 3 | Multiple features used together |
| Determinism Tests | 1 | Same input produces identical output |
| Edge Cases | 5 | Boundary conditions and Unicode handling |

### Implemented Features (Should Succeed)

| Test | Feature | Schema Reference |
|------|---------|------------------|
| `matrix_simple_text_query` | Basic query.text | docs/query-schema.md:34-50 |
| `matrix_tags_require_only` | tags.require (AND) | docs/query-schema.md:72-86 |
| `matrix_tags_require_any` | tags.requireAny (OR) | docs/query-schema.md:79-86 |
| `matrix_tags_exclude` | tags.exclude | docs/query-schema.md:90-100 |
| `matrix_tags_combined_filters` | All tag filters together | docs/query-schema.md:69-102 |
| `matrix_output_profile_balanced` | output.profile | docs/query-schema.md:265-267 |
| `matrix_output_profile_wide_alias_succeeds` | output.profile wide alias | docs/query-schema.md:265-267 |
| `matrix_output_explain_true` | output.explain | docs/query-schema.md:270 |
| `matrix_budget_max_tokens` | budget.maxTokens | docs/query-schema.md:286 |
| `matrix_budget_max_results` | budget.maxResults | docs/query-schema.md:287 |
| `matrix_query_mode_hybrid` | query.mode | docs/query-schema.md:49 |
| `matrix_time_after_filters_created_at` | time.after | docs/query-schema.md:139-156 |
| `matrix_time_before_accepts_open_ended_future_window` | time.before | docs/query-schema.md:139-156 |
| `matrix_as_of_future_snapshot_succeeds` | asOf | docs/query-schema.md:159-168 |
| `matrix_temporal_validity_strict_succeeds` | temporalValidity | docs/query-schema.md:171-186 |
| `matrix_trust_min_class_succeeds` | trust.minClass | docs/query-schema.md:192-217 |
| `matrix_redaction_respect_succeeds` | redaction.policy=respect | docs/query-schema.md:192-217 |
| `matrix_redaction_allow_categories_filters_secret_reasons` | redaction.allowCategories | docs/query-schema.md:192-217 |
| `matrix_graph_traversal_hints_expand_seed_neighborhood` | graph traversal, linkTypes, includeOrphans | docs/query-schema.md:221-258 |
| `matrix_graph_traversal_direction_and_orphan_handling` | inbound/outbound/bidirectional graph traversal | docs/query-schema.md:221-258 |
| `matrix_graph_hints_do_not_expand_cross_workspace_links` | graph workspace scope filtering | docs/query-schema.md:221-258 |
| `matrix_pagination_limit` | pagination.limit | docs/query-schema.md:299-318 |
| `matrix_pagination_cursor_first_page` | pagination.cursor | docs/query-schema.md:299-318 |

### Unimplemented Features (ERR_UNSUPPORTED_FEATURE)

| Test | Feature | Schema Reference |
|------|---------|------------------|
| `matrix_output_profile_custom_is_recognized_but_unsupported` | output.profile custom | docs/query-schema.md:265-267 |

Query modes other than `hybrid` still return `ERR_UNSUPPORTED_FEATURE` when
requested directly.

### Error Cases

| Test | Error Code | Trigger |
|------|------------|---------|
| `matrix_error_malformed_json` | ERR_MALFORMED_JSON | Invalid JSON syntax |
| `matrix_error_unknown_version` | ERR_UNKNOWN_VERSION | Unrecognized schema version |
| `matrix_error_empty_query_text` | ERR_EMPTY_QUERY | Empty string query |
| `matrix_error_whitespace_query_text` | ERR_EMPTY_QUERY | Whitespace-only query |
| `matrix_error_invalid_timestamp_format` | ERR_INVALID_TIMESTAMP | Non-ISO8601 timestamp |
| `matrix_error_zero_budget_max_tokens` | ERR_ZERO_BUDGET | maxTokens = 0 |
| `matrix_error_query_file_not_found` | ERR_QUERY_FILE_NOT_FOUND | Nonexistent file path |
| `matrix_error_tags_wrong_type_array` | ERR_MALFORMED_JSON | tags as array (not object) |
| `matrix_error_tags_wrong_type_string` | ERR_MALFORMED_JSON | tags as string (not object) |
| `matrix_pagination_invalid_cursor` | ERR_MALFORMED_JSON | Invalid pagination cursor |
| `matrix_pagination_zero_limit` | ERR_ZERO_BUDGET | pagination.limit = 0 |

### Determinism Guarantee

The `matrix_deterministic_output` test verifies that:
- Same workspace + same query file = identical item IDs
- Pack ordering is stable across runs
- No non-deterministic data leaks into output

This is required by AGENTS.md: "Given the same database, indexes, config, and query,
JSON output must be stable."

### Running the Matrix

```bash
# Run full conformance matrix
cargo test --test query_v1_matrix

# Run specific section
cargo test --test query_v1_matrix matrix_tags
cargo test --test query_v1_matrix matrix_error
cargo test --test query_v1_matrix matrix_graph

# With output for debugging
cargo test --test query_v1_matrix -- --nocapture
```

### When Features Are Implemented

When an `ERR_UNSUPPORTED_FEATURE` test transitions to working:

1. Move the test from Section 2 to Section 1
2. Change assertion from `assert_error_envelope` to `assert_response_envelope`
3. Add value assertions for the feature's behavior
4. Update this table to reflect the new status
5. Close the corresponding blocking bead

---

## Windows Installer Conformance: `install.ps1`

- **Script:** `install.ps1`
- **README Anchor:** `README.md` Windows PowerShell install snippet
- **Tracking Beads:** `bd-3tprq.1` defines the matrix; `bd-3tprq.2`
  implements the parser/static harness; `bd-3tprq.5` extends the docs/help
  drift guard.
- **Static Harness:** `scripts/windows-installer-static-check.ps1`
- **CI Job:** `windows-installer-static-conformance`

This matrix is the source of truth for Windows installer behavior before adding
parser, mocked-flow, or live-smoke tests. Rows distinguish deterministic offline
checks from tests that must run on Windows. Default conformance must not require
network access, a live GitHub release, or local Rust compilation.

### Coverage Matrix

| ID | Level | Requirement and rationale | Source anchors | Intended check and first planned artifact | Windows required | Current coverage / deviation |
|----|-------|---------------------------|----------------|-------------------------------------------|------------------|------------------------------|
| `WIN-PS1-001` | MUST | `install.ps1` remains PowerShell 5.1 parseable as UTF-8 and keeps the leading UTF-8 BOM. Windows PowerShell 5.1 handles non-ASCII script bytes differently from PowerShell 7, so the encoding contract is part of the installer surface. | `install.ps1:1`, `install.ps1:65-67`, `install.ps1:87` | Static bytes check for UTF-8 BOM plus Windows parser check in `scripts/windows-installer-static-check.ps1`. | Yes, PowerShell 5.1 | Covered by the `windows-installer-static-conformance` job using `powershell`. |
| `WIN-PS1-002` | MUST | The same script remains parseable on PowerShell 7+. The installer supports current cross-platform PowerShell while preserving Windows PowerShell 5.1 compatibility. | `install.ps1:65-67`, `install.ps1:87` | Windows parser check using `scripts/windows-installer-static-check.ps1` under `pwsh -NoProfile`. | Yes, PowerShell 7+ | Covered by the `windows-installer-static-conformance` job using `pwsh`. |
| `WIN-PS1-003` | MUST | README, script, and generated release-note examples download the release asset to a file with `Invoke-WebRequest -OutFile`; they must not document `iwr ... .Content \| iex` or scriptblock-created release-asset content. GitHub serves release assets as octet streams, so `.Content` is a byte array on Windows. | `install.ps1:56-60`, `README.md` Windows install snippet, `.github/workflows/release.yml` release-note generator | Offline docs/script/release-note check in `scripts/windows-installer-static-check.ps1` requiring `-OutFile` and forbidding executable byte-array release-asset examples. | No | Covered for README, installer examples, generated release-note examples, and the `bd-3tprq.5` docs/help verification-vocabulary drift guard. |
| `WIN-PS1-004` | MUST | Default release installs verify SHA256 unless `-NoVerify` or `EE_SKIP_VERIFY=1` is set. SHA256 is the mandatory integrity gate for normal Windows installs. | `install.ps1:7-8`, `install.ps1:42-48`, `install.ps1:90-94`, `install.ps1:901-915` | Mocked download/install flow in `bd-3tprq.3` asserting the checksum path runs by default. | Mockable offline; final proof on Windows | Planned by `bd-3tprq.3`. |
| `WIN-PS1-005` | MUST | `-NoVerify` skips both SHA256 and Sigstore checks. This escape hatch must be explicit and testable because it weakens integrity verification. | `install.ps1:42-48`, `install.ps1:901-938` | Mocked install flow in `bd-3tprq.3` asserting no checksum fetch, no `Test-Sha256`, and no Sigstore fetch. | Mockable offline; final proof on Windows | Planned by `bd-3tprq.3`. |
| `WIN-PS1-006` | SHOULD | Sigstore/SLSA verification is opt-in by default, after SHA256 succeeds. A missing bundle or missing `cosign` warns by default rather than failing the install. | `install.ps1:8-11`, `install.ps1:45-48`, `install.ps1:916-936` | Mocked missing-bundle and missing-cosign flows in `bd-3tprq.3`. | Mockable offline; final proof on Windows | Intentional POSIX parity: fatal provenance is only `-RequireProvenance` or `EE_REQUIRE_PROVENANCE=1`. |
| `WIN-PS1-007` | MUST | `-RequireProvenance` fails when the Sigstore bundle is unavailable. Required provenance cannot silently degrade. | `install.ps1:45-48`, `install.ps1:923-930` | Mocked bundle download failure with `-RequireProvenance` in `bd-3tprq.3`. | Mockable offline; final proof on Windows | Planned by `bd-3tprq.3`. |
| `WIN-PS1-008` | MUST | `-RequireProvenance` fails when `cosign` is absent. Required Sigstore verification cannot proceed without the verifier. | `install.ps1:513-516`, `install.ps1:920-936` | Mocked PATH/environment with no `cosign` in `bd-3tprq.3`; warning-only behavior remains valid without `-RequireProvenance`. | Mockable offline; final proof on Windows | Planned by `bd-3tprq.3`. |
| `WIN-PS1-009` | MUST | `EE_REQUIRE_PROVENANCE=1` enables the same fatal behavior as `-RequireProvenance`. Environment-driven policy must match the flag. | `install.ps1:90-94`, `install.ps1:926-936` | Mocked install flow with `EE_REQUIRE_PROVENANCE=1` and missing bundle/cosign in `bd-3tprq.3`. | Mockable offline; final proof on Windows | Planned by `bd-3tprq.3`. |
| `WIN-PS1-010` | MUST | `Show-AgentIntegration` cannot throw under `Set-StrictMode -Version Latest` when optional agent detection yields zero, one, or many scalar-like results. The installer must not report a false failure after installing `ee.exe`. | `install.ps1:725-783`, especially `install.ps1:772-777` | Static function-shape check in `scripts/windows-installer-static-check.ps1` proves the optional-agent `Where-Object` result is wrapped in `@()` before `.Count`; mocked runtime cases remain in `bd-3tprq.3`. | Yes | Static regression covered by `bd-3tprq.2`; runtime zero/one/many flow coverage remains planned by `bd-3tprq.3`. |
| `WIN-PS1-011` | SHOULD | Live GitHub release smoke is opt-in only. Default conformance stays deterministic, offline, and independent of current release availability. | README Windows snippet, `install.ps1:306-317`, `install.ps1:846-856` | Separate skipped-by-default live job gated by an env var such as `EE_INSTALLER_LIVE_SMOKE=1` in `bd-3tprq.4`. | Yes for live path | Planned by `bd-3tprq.4`. |
| `WIN-PS1-012` | MUST | Default installer tests do not compile Rust locally. The `-FromSource` path may be tested only through an approved remote/CI path or a mocked/static check. | `install.ps1:657-718`, AGENTS RCH policy | `scripts/windows-installer-static-check.ps1` is parser/static only; mocked release path remains the default for installer flow tests. | No for default checks | Static harness requires no Rust compilation, network, or live release. |

### Implementation Notes

- Offline deterministic checks are the default: byte inspection, parser checks,
  static docs/script drift checks, and mocked installer flows.
- Live GitHub release smoke belongs in a separately gated job and must log why it
  is skipped when the opt-in environment variable is absent.
- Windows Sigstore is intentionally opt-in after SHA256 by default, matching the
  current POSIX installer posture. Fatal provenance requires
  `-RequireProvenance` or `EE_REQUIRE_PROVENANCE=1`.
- `bd-3tprq.5` docs/help drift checks are intentionally folded into
  `scripts/windows-installer-static-check.ps1` so README snippets,
  installer help, generated release-note examples, and this matrix fail in one
  deterministic no-network harness when verification vocabulary drifts. The
  guard pins `-NoVerify`, `EE_SKIP_VERIFY=1`, `-RequireProvenance`, and
  `EE_REQUIRE_PROVENANCE=1` beside the `WIN-PS1-*` rows above.
- Source-install coverage must not run local Rust compilation from ordinary
  agent verification. Use an approved remote/CI lane or a mock/static harness.

---

## CASS Contracts Conformance

**Test File:** `conformance/cass_contracts.rs`  
**Schema Doc:** CASS robot/JSON output contracts

See `conformance/DISCREPANCIES.md` for known gaps.

---

## Adding New Conformance Suites

1. Create `conformance/<schema>_matrix.rs`
2. Add entry to this file documenting coverage
3. Link to schema documentation
4. Track blocking beads for unimplemented features
5. Include determinism test if schema promises stability
