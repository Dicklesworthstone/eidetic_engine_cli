//! Bind repairs to executed subjects in Rust, JavaScript, Python and checkers.
//!
//! Plain libtest output is projected into the bounded native event validator.
//! Complete external reports carry producer-specific subject commitments, never
//! stored evidence. All families share process-completion and poll guards.

#[cfg(test)]
use super::failure_diagnostics;
use super::{MAX_SCANNED_OUTPUT_BYTES, ToolOutput, compiler};

#[path = "cass_error_recall_plain_test_coverage.rs"]
mod plain;
#[path = "cass_error_recall_structured_test_coverage.rs"]
mod structured;
#[path = "cass_error_recall_js_tests.rs"]
pub(super) mod javascript;
#[path = "cass_error_recall_pytest.rs"]
pub(super) mod pytest;

enum ExternalExecution {
    Javascript(javascript::Execution),
    Pytest(pytest::Execution),
}

impl ExternalExecution {
    fn has_known_failures(&self) -> bool {
        match self {
            Self::Javascript(execution) => execution.has_known_failures(),
            Self::Pytest(execution) => execution.has_known_failures(),
        }
    }

    fn verified_by(&self, proof: &Self) -> bool {
        match (self, proof) {
            (Self::Javascript(required), Self::Javascript(proof)) => required.verified_by(proof),
            (Self::Pytest(required), Self::Pytest(proof)) => required.verified_by(proof),
            _ => false,
        }
    }
}

fn external_execution(text: &str) -> Result<Option<(bool, ExternalExecution)>, ()> {
    if let Some(decoded) = pytest::decode(text)? {
        return Ok(Some((
            decoded.output.allows_completion,
            ExternalExecution::Pytest(decoded.execution),
        )));
    }
    Ok(javascript::decode(text)?.map(|decoded| {
        (
            decoded.output.allows_completion,
            ExternalExecution::Javascript(decoded.execution),
        )
    }))
}

pub(super) struct Requirement(structured::Requirement, Option<ExternalExecution>);

impl Requirement {
    pub(super) fn capture(output: &ToolOutput, process_complete: bool) -> Self {
        match external_execution(&output.text) {
            Ok(Some((_, execution))) => {
                let required = (process_complete && execution.has_known_failures())
                    .then_some(execution);
                return Self(structured::Requirement::Unproven, required);
            }
            Err(()) => return Self(structured::Requirement::Unproven, None),
            Ok(None) => {}
        }
        Self(
            match plain::project(output) {
                Ok(Some(projected)) => structured::Requirement::capture(&projected, process_complete),
                Ok(None) => structured::Requirement::capture(output, process_complete),
                Err(()) => structured::Requirement::Unproven,
            },
            None,
        )
    }

    pub(super) fn is_guarded(&self) -> bool {
        self.0.is_guarded()
    }

    pub(super) fn refresh(&mut self, output: &ToolOutput, process_complete: bool) {
        match external_execution(&output.text) {
            Ok(Some((_, execution))) => {
                self.0 = structured::Requirement::Unproven;
                self.1 = (process_complete && execution.has_known_failures()).then_some(execution);
                return;
            }
            Err(()) => {
                self.0 = structured::Requirement::Unproven;
                self.1 = None;
                return;
            }
            Ok(None) => self.1 = None,
        }
        // Losing a typed report on a later poll must not weaken its coverage
        // requirement to generic command equality. The native arm retains it.
        match plain::project(output) {
            Ok(Some(projected)) => self.0.refresh(&projected, process_complete),
            Ok(None) => self.0.refresh(output, process_complete),
            Err(()) => self.0 = structured::Requirement::Unproven,
        }
    }

    pub(super) fn is_verified_by(&self, proof: &Proof) -> bool {
        match &self.1 {
            Some(required) => proof
                .1
                .as_ref()
                .is_some_and(|execution| required.verified_by(execution)),
            None => self.0.is_verified_by(&proof.0),
        }
    }
}

#[derive(Default)]
pub(super) struct Proof(structured::Proof, Option<ExternalExecution>);

impl Proof {
    pub(super) fn capture(output: &ToolOutput) -> Self {
        // The complete original output, including any timeout/truncation or
        // failure trailer, must pass the independent process-completion gate.
        if !output.succeeded() {
            return Self::default();
        }
        match external_execution(&output.text) {
            Ok(Some((allows_completion, execution))) => {
                return Self(
                    structured::Proof::default(),
                    allows_completion.then_some(execution),
                );
            }
            Err(()) => return Self::default(),
            Ok(None) => {}
        }
        Self(
            match plain::project(output) {
                Ok(Some(projected)) => structured::Proof::capture(&projected),
                Ok(None) => structured::Proof::capture(output),
                Err(()) => structured::Proof::default(),
            },
            None,
        )
    }
}
