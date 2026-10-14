//! Bind repairs to executed subjects in Rust, JavaScript, Python and checkers.
//!
//! Plain libtest output is projected into the bounded native event validator.
//! Complete external reports carry producer-specific subject commitments, never
//! stored evidence. All families share process-completion and poll guards.

#[cfg(test)]
use super::failure_diagnostics;
use super::{MAX_SCANNED_OUTPUT_BYTES, ToolOutput, compiler};

#[path = "cass_error_recall_js_tests.rs"]
pub(super) mod javascript;
#[path = "cass_error_recall_node_tap.rs"]
pub(super) mod node;
#[path = "cass_error_recall_plain_test_coverage.rs"]
mod plain;
#[path = "cass_error_recall_pytest.rs"]
pub(super) mod pytest;
#[path = "cass_error_recall_pytest_terminal/mod.rs"]
pub(super) mod python;
#[path = "cass_error_recall_sarif.rs"]
pub(super) mod sarif;
#[path = "cass_error_recall_structured_test_coverage.rs"]
mod structured;

enum ExternalExecution {
    Javascript(javascript::Execution),
    Node(node::Execution),
    Pytest(pytest::Execution),
    PytestTerminal(python::Execution),
    Sarif(sarif::Execution),
}

enum ExternalReport {
    CommandFailure,
    Tests {
        allows_completion: bool,
        execution: ExternalExecution,
    },
}

impl ExternalExecution {
    fn has_known_failures(&self) -> bool {
        match self {
            Self::Javascript(execution) => execution.has_known_failures(),
            Self::Node(execution) => execution.has_known_failures(),
            Self::Pytest(execution) => execution.has_known_failures(),
            Self::PytestTerminal(execution) => execution.has_known_failures(),
            Self::Sarif(execution) => execution.has_known_failures(),
        }
    }

    fn verified_by(&self, proof: &Self) -> bool {
        match (self, proof) {
            (Self::Javascript(required), Self::Javascript(proof)) => required.verified_by(proof),
            (Self::Node(required), Self::Node(proof)) => required.verified_by(proof),
            (Self::Pytest(required), Self::Pytest(proof)) => required.verified_by(proof),
            (Self::PytestTerminal(required), Self::PytestTerminal(proof)) => {
                required.verified_by(proof)
            }
            (Self::Sarif(required), Self::Sarif(proof)) => required.verified_by(proof),
            _ => false,
        }
    }
}

fn external_execution(text: &str) -> Result<Option<ExternalReport>, ()> {
    if let Some(decoded) = sarif::decode(text)? {
        return Ok(Some(ExternalReport::Tests {
            allows_completion: decoded.output.allows_completion,
            execution: ExternalExecution::Sarif(decoded.execution),
        }));
    }
    if let Some(decoded) = node::decode(text)? {
        return Ok(Some(ExternalReport::Tests {
            allows_completion: decoded.output.allows_completion,
            execution: ExternalExecution::Node(decoded.execution),
        }));
    }
    if let Some(decoded) = python::decode(text)? {
        // A command-level interpreter exception is not a test report with
        // missing inventory. Preserve the existing exact-invocation proof
        // path; refresh still cannot downgrade an already guarded test run.
        if decoded.execution.is_command_failure() {
            return Ok(Some(ExternalReport::CommandFailure));
        }
        return Ok(Some(ExternalReport::Tests {
            allows_completion: decoded.allows_completion,
            execution: ExternalExecution::PytestTerminal(decoded.execution),
        }));
    }
    if let Some(decoded) = pytest::decode(text)? {
        return Ok(Some(ExternalReport::Tests {
            allows_completion: decoded.output.allows_completion,
            execution: ExternalExecution::Pytest(decoded.execution),
        }));
    }
    Ok(
        javascript::decode(text)?.map(|decoded| ExternalReport::Tests {
            allows_completion: decoded.output.allows_completion,
            execution: ExternalExecution::Javascript(decoded.execution),
        }),
    )
}

pub(super) struct Requirement(structured::Requirement, Option<ExternalExecution>);

impl Requirement {
    pub(super) fn capture(output: &ToolOutput, process_complete: bool) -> Self {
        match external_execution(&output.text) {
            Ok(Some(ExternalReport::CommandFailure)) => {
                return Self(structured::Requirement::Other, None);
            }
            Ok(Some(ExternalReport::Tests { execution, .. })) => {
                let required =
                    (process_complete && execution.has_known_failures()).then_some(execution);
                return Self(structured::Requirement::Unproven, required);
            }
            Err(()) => return Self(structured::Requirement::Unproven, None),
            Ok(None) => {}
        }
        Self(
            match plain::project(output) {
                Ok(Some(projected)) => {
                    structured::Requirement::capture(&projected, process_complete)
                }
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
            Ok(Some(ExternalReport::CommandFailure)) => {
                // Do not parse rendered source code as a second test report,
                // or downgrade an earlier test requirement on a later poll.
                self.0 = if self.is_guarded() {
                    structured::Requirement::Unproven
                } else {
                    structured::Requirement::Other
                };
                self.1 = None;
                return;
            }
            Ok(Some(ExternalReport::Tests { execution, .. })) => {
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
            Ok(Some(ExternalReport::CommandFailure)) => return Self::default(),
            Ok(Some(ExternalReport::Tests {
                allows_completion,
                execution,
            })) => {
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
