# Workspace Hygiene

`ee workspace hygiene` is the commit-readiness diagnostic for shared agent
checkouts. The surface is read-only: it may inspect git status, Beads metadata,
Agent Mail coordination state, file metadata, and redacted secret-risk evidence,
but it must not stage, unstage, delete, move, stash, reset, checkout, or commit
files.

The diagnostic exists to help agents decide what is safe to include in a commit
slice. It does not replace human review, Beads ownership, or Agent Mail file
reservations.

## Buckets

Each dirty path is classified into one top-level bucket:

| Bucket | Meaning |
| --- | --- |
| `stage_candidate` | The path looks suitable for a commit slice, subject to later reservation and Beads overlays. |
| `do_not_commit` | The path is scratch, generated, local-machine state, or untracked secret-risk material. |
| `needs_human_review` | The path is tracked but risky, binary, oversized, or otherwise ambiguous. |
| `ignore_for_now` | The path is recognized as specialist metadata or unknown untracked material that the agent should leave alone. |

Downstream staging recommendations must treat `secret_risk` and active
coordination blockers as stronger than any local allow or stage pattern.

## Kinds

The path kind explains why the bucket was chosen:

| Kind | Typical paths |
| --- | --- |
| `source` | `src/**/*.rs`, `Cargo.toml`, `build.rs`, `migrations/**/*.sql` |
| `test` | `tests/**`, `benches/**`, `*_test.rs`, `*_tests.rs` |
| `docs` | `docs/**`, root `README*`, root `LICENSE*`, root Markdown |
| `beads_metadata` | `.beads/**`, especially `.beads/issues.jsonl` |
| `generated` | `target/**`, `Cargo.lock`, object and library artifacts |
| `scratch` | ad hoc reports, temporary probes, root helper output |
| `local_machine` | `.DS_Store`, AppleDouble files, local DB/log files |
| `secret_risk` | env files, key files, credential locations, redacted secret scan evidence |
| `binary` | large or binary paths where bounded content scan was skipped |
| `unknown` | anything not matched by the current taxonomy |

## Reason Codes

Reason codes are stable snake-case identifiers emitted in `reasons[]`.
They are intentionally short so agents can branch on them without parsing prose.

| Reason code | Kind | Default bucket |
| --- | --- | --- |
| `src_rust_source` | `source` | `stage_candidate` |
| `src_cargo_manifest` | `source` | `stage_candidate` |
| `src_build_script` | `source` | `stage_candidate` |
| `src_migration_sql` | `source` | `stage_candidate` |
| `tests_dir` | `test` | `stage_candidate` |
| `test_file_suffix` | `test` | `stage_candidate` |
| `bench_file` | `test` | `stage_candidate` |
| `docs_dir` | `docs` | `stage_candidate` |
| `docs_markdown_root` | `docs` | `stage_candidate` |
| `docs_license` | `docs` | `stage_candidate` |
| `docs_readme` | `docs` | `stage_candidate` |
| `beads_jsonl` | `beads_metadata` | `ignore_for_now` |
| `beads_dir` | `beads_metadata` | `ignore_for_now` |
| `generated_target_dir` | `generated` | `do_not_commit` |
| `generated_cargo_lock` | `generated` | `do_not_commit` |
| `generated_build_artifact` | `generated` | `do_not_commit` |
| `scratch_root_helper` | `scratch` | `do_not_commit` |
| `scratch_root_report` | `scratch` | `do_not_commit` |
| `scratch_root_tool_output` | `scratch` | `do_not_commit` |
| `scratch_root_tmp` | `scratch` | `do_not_commit` |
| `scratch_line_length_probe` | `scratch` | `do_not_commit` |
| `local_apple_double` | `local_machine` | `do_not_commit` |
| `local_ds_store` | `local_machine` | `do_not_commit` |
| `local_windows_shell` | `local_machine` | `do_not_commit` |
| `local_db_file` | `local_machine` | `do_not_commit` |
| `local_log_file` | `local_machine` | `do_not_commit` |
| `secret_path_pattern` | `secret_risk` | `do_not_commit` for untracked, `needs_human_review` for tracked |
| `secret_content_evidence` | `secret_risk` | `do_not_commit` for untracked, `needs_human_review` for tracked |
| `binary_large_file` | `binary` | `needs_human_review` |
| `binary_skip_reason` | `binary` | `needs_human_review` |
| `unknown_untracked` | `unknown` | `ignore_for_now` |
| `unknown_tracked` | `unknown` | `needs_human_review` |
| `secret_risk_overrides_tracked` | `secret_risk` | `needs_human_review` |

## Scratch Examples

Root-only scratch patterns are deliberately narrow. A file named `--help` in a
fixture directory may be meaningful test data; a root `--help` file is almost
always redirected command output.

Examples classified as `scratch` / `do_not_commit`:

```text
--help
ubs-report-2026-05-16.txt
ubs.json
ubs_findings.jsonl
ubs_full.txt
drift-report.txt
.plan-drift-report.json
critical.json
functions.txt
line-length-probe-output.txt
test_ln_1p
test_ln_1p.rs
test_multibyte
test_multibyte.rs
```

The classifier should add new root scratch patterns only when they are common
agent or verifier artifacts. It should not classify every root `.json`, `.txt`,
or `.rs` file as scratch by extension alone.

## Configuration

The classifier accepts caller-supplied generated, scratch, local-machine, and
always-review path patterns in addition to the built-in defaults. The pure core
classifier consumes normalized matchers only; CLI/config code is responsible for
parsing `EE_*` values from the registry.

Registered environment variables:

| Variable | Effect |
| --- | --- |
| `EE_WORKSPACE_HYGIENE_GENERATED_PATTERNS` | Adds generated-artifact path matchers. |
| `EE_WORKSPACE_HYGIENE_SCRATCH_PATTERNS` | Adds scratch-artifact path matchers. |
| `EE_WORKSPACE_HYGIENE_LOCAL_MACHINE_PATTERNS` | Adds machine-local artifact path matchers. |
| `EE_WORKSPACE_HYGIENE_ALWAYS_REVIEW_PATTERNS` | Forces matching paths into `needs_human_review`. |

Matcher syntax is `exact:<path>`, `prefix:<path>`, `suffix:<path>`, or
`contains:<text>`, with comma separation for multiple matchers. Patterns are
repo-relative and should stay narrow.

Local configuration must never mark secret-risk files safe. Secret-risk evidence,
active reservations, and Beads ownership always override local stage preferences.
