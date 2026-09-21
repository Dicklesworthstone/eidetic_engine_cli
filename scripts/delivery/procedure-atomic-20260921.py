#!/usr/bin/env python3
"""One-time delivery of the reviewed atomic procedure-learning change.

All targets are pinned to reviewed source identities. Refuse changed files;
never delete, reset, rebase, switch branches, or rewrite unrelated source.
Rustfmt parses edited units only; it is not a compilation/test verdict.
"""
import hashlib
from pathlib import Path
import subprocess

EXPECTED = {
    "src/db/mod.rs": "8aa66a8e38e8caf595388230791c9ed8b288b287",
    "src/core/outcome.rs": "4c3efeb2bc7359f9de514ff68e8a7e3328d3123f",
}
TEST_SOURCE = "scripts/delivery/procedure-atomic-20260921-tests.txt"
TEST_TARGET = "src/core/outcome_procedure_atomic_tests.rs"
TEST_BLOB = "0d74255dbf79c4f3893b4272e453446592703736"


def blob(content):
    return hashlib.sha1(b"blob " + str(len(content)).encode() + b"\0" + content).hexdigest()


def replace_once(text, old, new):
    assert old and text.count(old) == 1, "reviewed source fragment changed"
    return text.replace(old, new, 1)


def formatted(text):
    command = ["rustfmt", "--edition", "2024", "--config", "skip_children=true", "--emit", "stdout"]
    first = subprocess.run(command, input=text, text=True, capture_output=True, check=True).stdout
    second = subprocess.run(command, input=first, text=True, capture_output=True, check=True).stdout
    assert first == second, "edited Rust unit is not format-stable"
    return first


def database_method(text):
    start_marker = "    /// Apply one feedback signal to a procedure and optionally auto-retire it.\n"
    end_marker = "\n    /// Get a feedback event by its ID."
    assert text.count(start_marker) == 1
    start = text.index(start_marker)
    end = text.index(end_marker, start)
    old = text[start:end]
    assert hashlib.sha256(old.encode()).hexdigest() == "975b46951048bca42e1243e2fb611dcb8b3a4d4e48f9e92cbc619184fb109303"
    opening = "        self.with_transaction(|| {\n"
    closing = "        })\n    }\n"
    assert old.count(opening) == 1 and old.endswith(closing)
    signature, body = old.split(opening)
    body = body[:-len(closing)]
    assert all(not line.strip() or line.startswith("    ") for line in body.splitlines())
    body = "".join(line[4:] if line.strip() else line for line in body.splitlines(keepends=True))
    replacement = signature + "        self.with_transaction(|| self.apply_procedure_feedback_in_txn(input))\n    }\n\n"
    replacement += METHOD_HEADER
    replacement += body + "    }\n"
    # This is exactly the method extraction from the prepared bundle before
    # formatting; arithmetic, signal policy, and history writes are unchanged.
    assert hashlib.sha256(replacement.encode()).hexdigest() == "af6daf38029bfce0772154facf58aa9f9c7dcd1d910a3fa51ef7283310a14f5f"
    return old, replacement


METHOD_HEADER = """    /// Apply procedure learning in the caller's already-open transaction.
    ///
    /// Outcome recording must commit its feedback idempotency key, audit,
    /// counters and retirement history together. Do not open or commit a
    /// nested transaction here; standalone callers use the public wrapper.
    pub(crate) fn apply_procedure_feedback_in_txn(
        &self,
        input: ApplyProcedureFeedbackInput<'_>,
    ) -> Result<Option<ProcedureFeedbackUpdate>> {
"""
HELPER = """/// Apply procedure scoring in the transaction that records its feedback.
///
/// Standalone procedure callers retain their transactional DB wrapper. The
/// outcome path uses the caller-owned variant so a history-write failure also
/// rolls back the feedback event and its audit, leaving its ID retryable.
fn apply_procedure_outcome_in_txn(
    connection: &DbConnection,
    feedback: &CreateFeedbackEventInput,
    event_id: &str,
    actor: Option<&str>,
) -> crate::db::Result<()> {
    if feedback.target_type != "procedure" {
        return Ok(());
    }
    let missing_target = || crate::db::DbError::MalformedRow {
        operation: crate::db::DbOperation::Execute,
        message: "Outcome procedure disappeared before learning could commit".to_owned(),
    };
    // Target resolution happened before writer acquisition. Recheck ownership
    // inside the transaction, including neutral signals that do not score.
    connection
        .get_procedure(&feedback.workspace_id, &feedback.target_id)?
        .ok_or_else(missing_target)?;
    let update = connection.apply_procedure_feedback_in_txn(ApplyProcedureFeedbackInput {
        workspace_id: &feedback.workspace_id,
        procedure_id: &feedback.target_id,
        signal: &feedback.signal,
        weight: feedback.weight,
        auto_retire_harmful_threshold: 3,
        event_id: &procedure_event_id_for_feedback(event_id),
        reason: feedback.reason.as_deref(),
        actor,
    })?;
    if update.is_none()
        && (HELPFUL_SIGNALS.contains(&feedback.signal.as_str())
            || is_harmful_signal(&feedback.signal))
    {
        return Err(missing_target());
    }
    Ok(())
}

"""
OLD_POST_COMMIT = """    if target_type == "procedure" {
        connection
            .apply_procedure_feedback(ApplyProcedureFeedbackInput {
                workspace_id: &target.workspace_id,
                procedure_id: &target_id,
                signal: &signal,
                weight,
                auto_retire_harmful_threshold: 3,
                event_id: &procedure_event_id_for_feedback(&event_id),
                reason: feedback_input.reason.as_deref(),
                actor: options.actor.as_deref(),
            })
            .map_err(|error| DomainError::Storage {
                message: format!("Failed to update procedure feedback score: {error}"),
                repair: Some("ee procedure show <id> --json".to_string()),
            })?;
    }
"""
NEW_TXN_CALL = """                    apply_procedure_outcome_in_txn(
                        &connection,
                        &feedback_input,
                        &event_id,
                        options.actor.as_deref(),
                    )?;
                    Ok((audit_id, confidence))"""
OLD_DECLARATION = """#[cfg(test)]
#[path = "outcome_atomic_learning_tests.rs"]
mod atomic_learning_tests;
"""
NEW_DECLARATION = """#[cfg(test)]
#[path = "outcome_atomic_learning_tests.rs"]
mod atomic_learning_tests;

#[cfg(test)]
#[path = "outcome_procedure_atomic_tests.rs"]
mod procedure_atomic_tests;
"""


def main():
    paths = [*EXPECTED, TEST_TARGET]
    def git(*args):
        return subprocess.check_output(["git", *args], text=True).strip()
    assert git("branch", "--show-current") == "main", "not the main checkout"
    assert not git("status", "--porcelain", "--", *paths), "targeted source files are dirty"
    for name in [*paths, TEST_SOURCE]:
        path = Path(name)
        assert not any(part.is_symlink() for part in (path, *path.parents)), "symlinked source path"
    assert not Path(TEST_TARGET).exists(), "test target already exists"
    original = {}
    for name, expected in EXPECTED.items():
        content = Path(name).read_bytes()
        assert blob(content) == expected, "source changed; review required: " + name
        original[name] = content.decode("utf-8")
    tests = Path(TEST_SOURCE).read_bytes()
    assert blob(tests) == TEST_BLOB, "prepared test bytes changed"
    assert tests.count(b"#[test]") == 9 and b"#[ignore" not in tests

    db_old, db_new = database_method(original["src/db/mod.rs"])
    outcome = original["src/core/outcome.rs"]
    outcome = replace_once(outcome, OLD_POST_COMMIT, "")
    outcome = replace_once(outcome, "                    Ok((audit_id, confidence))", NEW_TXN_CALL)
    helper_anchor = "/// Apply memory learning inside the transaction that records its evidence.\n"
    outcome = replace_once(outcome, helper_anchor, HELPER + helper_anchor)
    outcome = replace_once(outcome, OLD_DECLARATION, NEW_DECLARATION)
    assert blob(outcome.encode()) == "564790914a42010f39f8ea8f83dcb31e869e236b", "prepared outcome candidate differs"

    wrapper = "impl DbConnection {\n"
    db_formatted = formatted(wrapper + db_new + "}\n")
    assert db_formatted.startswith(wrapper) and db_formatted.endswith("}\n")
    db_formatted = db_formatted[len(wrapper):-2]
    helper_formatted = formatted(HELPER).rstrip() + "\n\n"
    planned = {
        "src/db/mod.rs": replace_once(original["src/db/mod.rs"], db_old, db_formatted),
        "src/core/outcome.rs": replace_once(outcome, HELPER, helper_formatted),
        TEST_TARGET: formatted(tests.decode("utf-8")),
    }
    # All checks and Rust parsing succeed before any repository source write.
    for name, text in planned.items():
        Path(name).write_text(text, encoding="utf-8")
        print("APPLIED", name, blob(text.encode()))


if __name__ == "__main__":
    main()
