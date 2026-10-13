//! Bind repairs to executed subjects in structured and ordinary libtest output.
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

pub(super) struct Requirement(structured::Requirement);

impl Requirement {
    pub(super) fn capture(output: &ToolOutput, process_complete: bool) -> Self {
        // A typed JavaScript failure must not fall back to generic invocation
        // equality. Until subject coverage is available, retain the error but
        // abstain from asserting that another report proves its repair.
        if !matches!(javascript::inspect(&output.text), Ok(None)) {
            return Self(structured::Requirement::Unproven);
        }
        Self(match plain::project(output) {
            Ok(Some(projected)) => structured::Requirement::capture(&projected, process_complete),
            Ok(None) => structured::Requirement::capture(output, process_complete),
            Err(()) => structured::Requirement::Unproven,
        })
    }

    pub(super) fn is_guarded(&self) -> bool {
        self.0.is_guarded()
    }

    pub(super) fn refresh(&mut self, output: &ToolOutput, process_complete: bool) {
        if !matches!(javascript::inspect(&output.text), Ok(None)) {
            self.0 = structured::Requirement::Unproven;
            return;
        }
        match plain::project(output) {
            Ok(Some(projected)) => self.0.refresh(&projected, process_complete),
            Ok(None) => self.0.refresh(output, process_complete),
            Err(()) => self.0 = structured::Requirement::Unproven,
        }
    }

    pub(super) fn is_verified_by(&self, proof: &Proof) -> bool {
        self.0.is_verified_by(&proof.0)
    }
}

#[derive(Default)]
pub(super) struct Proof(structured::Proof);

impl Proof {
    pub(super) fn capture(output: &ToolOutput) -> Self {
        // Check the COMPLETE original output first. Projection must not hide
        // a timeout, truncation, failure trailer or still-running process.
        if !output.succeeded() {
            return Self::default();
        }
        Self(match plain::project(output) {
            Ok(Some(projected)) => structured::Proof::capture(&projected),
            Ok(None) => structured::Proof::capture(output),
            Err(()) => structured::Proof::default(),
        })
    }
}
