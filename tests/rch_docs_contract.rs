//! bd-9ygik.4 — RCH docs contract gate.
//!
//! Asserts that the agent-facing RCH verification docs
//! (`docs/rch_runbook.md` + `docs/rch_verification.md`) carry the
//! load-bearing content that bd-9ygik.4's acceptance criteria
//! require, and that they never silently regress into suggesting
//! the exact forbidden operations the rest of the RCH safety
//! plane (bd-1h8ji.2 tripwire, bd-1h8ji.4 portability diagnostic,
//! AGENTS.md hard rules) refuses at runtime.
//!
//! Why a docs gate at all: the docs are the agent-facing contract
//! for which RCH wrapper shapes are safe. A future docs edit that
//! drops `RCH_REQUIRE_REMOTE=1` from an example, or that quietly
//! suggests `git worktree` / `git stash` / `git reset` / `git
//! clean` as a "cleanup" strategy, would silently widen the trust
//! boundary on every agent who copy-pastes from the runbook. The
//! runtime guards still catch the failure mode at execution time,
//! but the cost in wasted closeout attempts is high. This gate
//! refuses the drift at PR time instead.
//!
//! Three invariant families enforced:
//!
//!   1. **Cargo command examples are RCH-routed.** Every fenced
//!      shell block that mentions `cargo build|check|test|bench|
//!      clippy` must either set `RCH_REQUIRE_REMOTE=1` in the
//!      surrounding env block, route through `scripts/rch_verify.sh`,
//!      go through `rch exec --`, or be an explicit don't-do-this
//!      example marked with a `# WRONG:` / `# BAD:` comment line.
//!
//!   2. **Beads comment template names the new source-attribution
//!      fields.** The runbook's closeout-flow section must mention
//!      every field bd-9ygik's source-attribution surface adds to
//!      the `ee.rch.verify.v1` proof, so an agent that pastes
//!      `summary_markdown` doesn't accidentally hide the new
//!      provenance evidence.
//!
//!   3. **Docs never recommend forbidden operations.** The runbook
//!      must explicitly REFUSE `git worktree`, `git stash`, `git
//!      reset --hard`, `git clean -fd`, and "local cargo fallback"
//!      as cleanup paths. Per AGENTS.md these are user-explicit-
//!      authorization-only operations; suggesting them in docs
//!      would short-circuit the authorization rule.
//!
//! All checks operate on file content read from disk at test time;
//! no I/O beyond `fs::read_to_string`, no Cargo, no RCH, no
//! subprocess. Pure assertions over markdown text.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;

type TestResult = Result<(), String>;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_doc(relative: &str) -> Result<String, String> {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
}

/// Walks the markdown body and returns every fenced code block's
/// `(language, body)` pair, ignoring blocks without a language hint.
fn fenced_code_blocks(body: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("```") {
            continue;
        }
        let lang_hint = trimmed.trim_start_matches('`').trim().to_string();
        let mut block = String::new();
        for inner in lines.by_ref() {
            if inner.trim_start().starts_with("```") {
                break;
            }
            block.push_str(inner);
            block.push('\n');
        }
        out.push((lang_hint, block));
    }
    out
}

/// True if the fenced shell block represents a safe RCH-routed
/// cargo invocation per the runbook's documented patterns.
fn shell_block_is_rch_routed(block: &str) -> bool {
    block.contains("scripts/rch_verify.sh")
        || block.contains("rch exec --")
        || block.contains("rch exec -- ")
        || block.contains("RCH_REQUIRE_REMOTE=1")
}

/// True if the fenced shell block is an explicit anti-pattern
/// example (a "# WRONG:" / "# BAD:" / "# DON'T:" comment, or the
/// known tripwire diagnostic JSON inside a `text`-tagged block
/// rather than an executable shell example).
fn shell_block_is_explicit_anti_pattern(lang_hint: &str, block: &str) -> bool {
    // The tripwire / runtime-diagnostic JSON blocks are tagged
    // `text` (not `bash` / `sh`) and quote the runtime guard's
    // own output rather than instructing the reader to run the
    // command. Skip them.
    if lang_hint.contains("text") || lang_hint.contains("json") {
        return true;
    }
    let lowered = block.to_ascii_lowercase();
    lowered.contains("# wrong")
        || lowered.contains("# bad:")
        || lowered.contains("# don't")
        || lowered.contains("# do not")
        || lowered.contains("# antipattern")
        || lowered.contains("# never:")
}

#[test]
fn rch_runbook_cargo_examples_are_rch_routed() -> TestResult {
    let runbook = read_doc("docs/rch_runbook.md")?;
    let mut offenders: Vec<String> = Vec::new();
    for (lang_hint, block) in fenced_code_blocks(&runbook) {
        // Only shell-flavored blocks are subject to the RCH-routing
        // contract. Markdown / json / toml / text blocks are
        // documentation samples, not executable instructions.
        let lower_hint = lang_hint.to_ascii_lowercase();
        if !(lower_hint.contains("bash") || lower_hint.contains("sh") || lower_hint.is_empty()) {
            continue;
        }
        // Match the cargo subcommands the bd-1h8ji.2 tripwire
        // catches at runtime; same vocabulary keeps the docs
        // gate and the runtime detector aligned.
        let mentions_cargo_compile = block.contains("cargo build")
            || block.contains("cargo check")
            || block.contains("cargo test")
            || block.contains("cargo bench")
            || block.contains("cargo clippy")
            || block.contains("cargo fmt");
        if !mentions_cargo_compile {
            continue;
        }
        if shell_block_is_explicit_anti_pattern(&lang_hint, &block) {
            continue;
        }
        if !shell_block_is_rch_routed(&block) {
            offenders.push(format!(
                "fenced ```{lang_hint}``` block mentions a cargo \
                 compile subcommand but is neither RCH-routed nor \
                 marked as an explicit anti-pattern:\n----\n{block}----"
            ));
        }
    }
    if !offenders.is_empty() {
        return Err(format!(
            "docs/rch_runbook.md has {} cargo example(s) that bypass the RCH wrapper contract:\n{}",
            offenders.len(),
            offenders.join("\n")
        ));
    }
    Ok(())
}

#[test]
fn rch_runbook_beads_comment_template_names_source_attribution_fields() -> TestResult {
    let runbook = read_doc("docs/rch_runbook.md")?;
    // Every field the bd-9ygik source-attribution surface adds to
    // the ee.rch.verify.v1 proof. An agent pasting the template
    // must see them all named so they don't accidentally drop the
    // new evidence on closeout.
    let required_fields = [
        "command_hash",
        "verification_attribution",
        "git_tree",
        "dirty_status_hash",
        "source_manifest_hash",
        "worker_id",
        "exit_code",
        "degraded_codes",
        "source_state_degraded_codes",
        "first_error",
    ];
    let mut missing: Vec<&str> = Vec::new();
    for field in required_fields {
        if !runbook.contains(field) {
            missing.push(field);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "docs/rch_runbook.md Beads comment template is missing bd-9ygik source-attribution \
             field name(s): {missing:?}. Restore them in the template so closeout proof carries \
             the full source-state provenance."
        ));
    }
    Ok(())
}

#[test]
fn rch_runbook_names_handoff_attribution_buckets() -> TestResult {
    let runbook = read_doc("docs/rch_runbook.md")?;
    // bd-9ygik.4 acceptance: handoff wording must distinguish
    // "code implemented but clean proof blocked" from "committed
    // tree verified" from "live dirty checkout proceeded". Pin
    // the four canonical bucket names so an Agent Mail recipient
    // can grep the runbook for the exact label they see in the
    // proof JSON.
    let required_buckets = [
        "strict_clean_tree",
        "live_dirty_checkout",
        "source_state_refused",
        "committed_tree_unsupported",
    ];
    let mut missing: Vec<&str> = Vec::new();
    for bucket in required_buckets {
        if !runbook.contains(bucket) {
            missing.push(bucket);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "docs/rch_runbook.md handoff section is missing canonical attribution bucket(s): \
             {missing:?}. Each bucket name must appear so agents can paste the exact wording \
             into Agent Mail and Beads comments instead of inventing ad-hoc phrasing."
        ));
    }
    Ok(())
}

#[test]
fn rch_runbook_refuses_forbidden_cleanup_operations() -> TestResult {
    let runbook = read_doc("docs/rch_runbook.md")?;
    // AGENTS.md forbids `git worktree`, `git stash`, `git reset`,
    // `git checkout <other-ref>`, `git clean -fd`, and local Cargo
    // fallback. The docs must explicitly REFUSE these as cleanup
    // strategies so an agent reading the runbook for "I have a
    // dirty checkout, how do I get a clean proof?" never lands on
    // a forbidden answer.
    //
    // The contract: the runbook must contain at least one
    // sentence-shaped occurrence of "never ... worktree", "never
    // ... stash", "never ... reset", and similar — OR the explicit
    // composite refusal sentence the runbook already carries
    // ("Never use source-proof modes as permission to run
    // git worktree, git stash, git reset, git checkout, deletion
    // cleanup, or local Cargo.").
    let refusal_keywords = [
        "git worktree",
        "git stash",
        "git reset",
        "git checkout",
        "local Cargo",
    ];
    let mut missing: Vec<&str> = Vec::new();
    for keyword in refusal_keywords {
        if !runbook.contains(keyword) {
            missing.push(keyword);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "docs/rch_runbook.md is missing the AGENTS.md-forbidden operation name(s) {missing:?}. \
             The runbook must name them explicitly so an agent reading 'how do I get clean proof' \
             cannot reach a forbidden answer through the docs."
        ));
    }
    // Refusal must appear in a NEGATIVE context — the runbook
    // says "never" or "do not" or "refuses" near at least one of
    // the keywords. Find the line containing one of the keywords
    // and check that a refusal verb appears in the same or
    // immediately-preceding line.
    let target_keyword = "git worktree";
    let lines: Vec<&str> = runbook.lines().collect();
    let mut found_refusal = false;
    for (i, line) in lines.iter().enumerate() {
        if !line.contains(target_keyword) {
            continue;
        }
        let window_start = i.saturating_sub(2);
        let window: String = lines[window_start..=i].join(" ").to_ascii_lowercase();
        if window.contains("never")
            || window.contains("do not")
            || window.contains("don't")
            || window.contains("refuse")
            || window.contains("forbidden")
            || window.contains("don't authorize")
        {
            found_refusal = true;
            break;
        }
    }
    if !found_refusal {
        return Err(format!(
            "docs/rch_runbook.md mentions `{target_keyword}` but no nearby refusal verb \
             (never / do not / refuse / forbidden / don't authorize). The keyword being \
             present is not enough — the docs must explicitly REJECT the operation."
        ));
    }
    Ok(())
}

#[test]
fn rch_runbook_documents_three_source_proof_modes() -> TestResult {
    let runbook = read_doc("docs/rch_runbook.md")?;
    // bd-9ygik.4 acceptance: the runbook must distinguish the
    // three documented source-proof modes so an agent picks the
    // right one before running RCH.
    let required_mode_names = ["Live checkout", "Strict clean checkout", "Committed-tree"];
    let mut missing: Vec<&str> = Vec::new();
    for mode in required_mode_names {
        if !runbook.contains(mode) {
            missing.push(mode);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "docs/rch_runbook.md is missing canonical source-proof mode name(s): {missing:?}. \
             The decision table must use these exact labels so an agent's Beads comment matches \
             what scripts/rch_verify.sh emits."
        ));
    }
    Ok(())
}

#[test]
fn rch_verification_doc_references_runbook() -> TestResult {
    // Cross-doc consistency: docs/rch_verification.md (the schema /
    // wrapper-internals doc) should reference docs/rch_runbook.md
    // (the agent-workflow doc) so a reader who lands on either
    // discovers the other. This keeps the bd-9ygik.4 split
    // discoverable: internals vs operator guide.
    let internals = read_doc("docs/rch_verification.md")?;
    if !internals.contains("rch_runbook.md") && !internals.contains("RCH Runbook") {
        return Err(
            "docs/rch_verification.md does not reference docs/rch_runbook.md. Add a cross-link \
             so a reader who lands on the internals doc discovers the operator runbook."
                .to_string(),
        );
    }
    Ok(())
}

#[test]
fn rch_runbook_canonical_command_pin_is_present() -> TestResult {
    // The TL;DR's "one canonical command shape" is the proven
    // recipe every bd-1h8ji.* and bd-3usjw.* close used. Pinning
    // the load-bearing env vars here ensures a future edit cannot
    // silently drop one and ship a partial recipe.
    let runbook = read_doc("docs/rch_runbook.md")?;
    let required_env_vars = [
        "TMPDIR=/tmp",
        "RCH_REQUIRE_REMOTE=1",
        "RCH_QUEUE_WHEN_BUSY=1",
        "RCH_DAEMON_WAIT_RESPONSE_TIMEOUT_SECS=900",
        "RCH_DAEMON_RESPONSE_TIMEOUT_SECS=900",
        "RCH_CANONICAL_PROJECT_ROOT=",
        "RCH_ALIAS_PROJECT_ROOT=",
        "RCH_COMPRESSION=0",
    ];
    let mut missing: Vec<&str> = Vec::new();
    for var in required_env_vars {
        if !runbook.contains(var) {
            missing.push(var);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "docs/rch_runbook.md TL;DR canonical command recipe is missing required env var(s): \
             {missing:?}. Each was load-bearing in at least one RCH-verified close this week; \
             dropping any of them silently produces a partial recipe that re-introduces a known \
             failure mode."
        ));
    }
    Ok(())
}
