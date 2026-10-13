//! Bind repairs to executed subjects in Rust, JavaScript and checker output.
//!
//! The plain formatter is projected into the existing bounded libtest event
//! validator. Both formats therefore share identity, inventory, fail-fast and
//! skipped-test rules; the projection never becomes stored evidence.

#[cfg(test)]
use super::failure_diagnostics;
use super::{MAX_SCANNED_OUTPUT_BYTES, ToolOutput, compiler};

#[path = "cass_error_recall_plain_test_coverage.rs"]
mod plain;
#[path = "cass_error_recall_structured_test_coverage.rs"]
mod structured;
#[path = "cass_error_recall_js_tests.rs"]
pub(super) mod javascript;

pub(super) struct Requirement(structured::Requirement, Option<javascript::Execution>);

impl Requirement {
    pub(super) fn capture(output: &ToolOutput, process_complete: bool) -> Self {
        match javascript::decode(&output.text) {
            Ok(Some(decoded)) => {
                let required = (process_complete && decoded.execution.has_known_failures())
                    .then_some(decoded.execution);
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
        match javascript::decode(&output.text) {
            Ok(Some(decoded)) => {
                self.0 = structured::Requirement::Unproven;
                self.1 = (process_complete && decoded.execution.has_known_failures())
                    .then_some(decoded.execution);
                return;
            }
            Err(()) => {
                self.0 = structured::Requirement::Unproven;
                self.1 = None;
                return;
            }
            Ok(None) => self.1 = None,
        }
        // Preserve the native guard when a later poll loses typed coverage.
        // JavaScript requirements keep an Unproven native arm for this reason.
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
pub(super) struct Proof(structured::Proof, Option<javascript::Execution>);

impl Proof {
    pub(super) fn capture(output: &ToolOutput) -> Self {
        // Check the COMPLETE original output first. Projection must not hide
        // a timeout, truncation, failure trailer or still-running process.
        if !output.succeeded() {
            return Self::default();
        }
        match javascript::decode(&output.text) {
            Ok(Some(decoded)) => {
                return Self(
                    structured::Proof::default(),
                    decoded.output.allows_completion.then_some(decoded.execution),
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
