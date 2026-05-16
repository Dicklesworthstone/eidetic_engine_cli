//! Diagnostic timing types for the ee.response.v1 meta field.
//!
//! When `--meta` is passed, the response envelope includes timing data
//! to help diagnose performance issues and understand execution phases.

use std::time::{Duration, Instant};

/// Timing metadata for diagnostic output.
///
/// Captures wall-clock elapsed time and optional phase breakdowns
/// for commands that support detailed timing.
#[derive(Clone, Debug, Default)]
pub struct DiagnosticTiming {
    /// Total wall-clock time for the command.
    pub elapsed_ms: f64,
    /// Optional breakdown by phase name.
    pub phases: Vec<TimingPhase>,
}

/// A named timing phase within command execution.
#[derive(Clone, Debug)]
pub struct TimingPhase {
    /// Phase name (e.g., "gather", "render", "query").
    pub name: &'static str,
    /// Duration of this phase in milliseconds.
    pub duration_ms: f64,
}

impl DiagnosticTiming {
    /// Create timing data with only elapsed time (no phase breakdown).
    #[must_use]
    pub fn elapsed_only(elapsed: Duration) -> Self {
        Self {
            elapsed_ms: elapsed.as_secs_f64() * 1000.0,
            phases: Vec::new(),
        }
    }

    /// Create timing data with phases.
    #[must_use]
    pub fn with_phases(elapsed: Duration, phases: Vec<TimingPhase>) -> Self {
        Self {
            elapsed_ms: elapsed.as_secs_f64() * 1000.0,
            phases,
        }
    }

    /// Check if this timing has phase breakdown.
    #[must_use]
    pub fn has_phases(&self) -> bool {
        !self.phases.is_empty()
    }
}

impl TimingPhase {
    /// Create a new timing phase.
    #[must_use]
    pub fn new(name: &'static str, duration: Duration) -> Self {
        Self {
            name,
            duration_ms: duration.as_secs_f64() * 1000.0,
        }
    }
}

/// Helper for measuring command execution timing with phase breakdowns.
///
/// Phases are demarcated by [`mark`](Self::mark) calls. Each `mark(name)` closes
/// the current phase and labels the segment that just ended with `name`. The
/// first phase starts implicitly at [`start`](Self::start), so a typical pattern
/// is:
///
/// ```ignore
/// let mut capture = TimingCapture::start();
/// /* ... do gather work ... */
/// capture.mark("gather");      // segment start..now is named "gather"
/// /* ... do render work ... */
/// capture.mark("render");      // segment previous-mark..now is named "render"
/// let timing = capture.finish();
/// ```
///
/// If any wall-clock time elapses between the last `mark` and `finish`, an
/// extra trailing phase named `"finish"` is appended to account for it.
#[derive(Debug)]
pub struct TimingCapture {
    start: Instant,
    phases: Vec<(Instant, &'static str)>,
}

impl TimingCapture {
    /// Start capturing timing.
    #[must_use]
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
            phases: Vec::new(),
        }
    }

    /// Close the current phase and label the segment that just ended.
    ///
    /// The named segment runs from the previous `mark` (or [`start`](Self::start)
    /// if this is the first call) to this call. Despite the name, this records
    /// the *end* of a phase, not the start of the next one.
    pub fn mark(&mut self, phase_name: &'static str) {
        self.phases.push((Instant::now(), phase_name));
    }

    /// Finish capturing and produce timing data.
    ///
    /// Each registered `mark(name)` becomes a `TimingPhase` named `name` whose
    /// duration is the elapsed time since the previous mark (or
    /// [`start`](Self::start) for the first one). If any time elapses between
    /// the final mark and this call, a trailing `"finish"` phase is appended.
    #[must_use]
    pub fn finish(self) -> DiagnosticTiming {
        let end = Instant::now();
        let elapsed = end.checked_duration_since(self.start).unwrap_or(Duration::ZERO);

        if self.phases.is_empty() {
            return DiagnosticTiming::elapsed_only(elapsed);
        }

        let mut phases = Vec::with_capacity(self.phases.len() + 1);
        let mut prev = self.start;

        for (instant, name) in &self.phases {
            let duration = instant.checked_duration_since(prev).unwrap_or(Duration::ZERO);
            phases.push(TimingPhase::new(name, duration));
            prev = *instant;
        }

        // Trailing segment from the last mark to `end`. We only include it
        // when measurable time elapsed so a tightly-paired mark/finish pair
        // doesn't pollute the breakdown with a zero-duration "finish" entry.
        let final_duration = end.checked_duration_since(prev).unwrap_or(Duration::ZERO);
        if final_duration.as_nanos() > 0 {
            phases.push(TimingPhase::new("finish", final_duration));
        }

        DiagnosticTiming::with_phases(elapsed, phases)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    type TestResult = Result<(), String>;

    fn ensure<T: PartialEq + std::fmt::Debug>(actual: T, expected: T, ctx: &str) -> TestResult {
        if actual == expected {
            Ok(())
        } else {
            Err(format!("{ctx}: expected {expected:?}, got {actual:?}"))
        }
    }

    #[test]
    fn elapsed_only_creates_timing_without_phases() -> TestResult {
        let timing = DiagnosticTiming::elapsed_only(Duration::from_millis(42));
        ensure(timing.elapsed_ms, 42.0, "elapsed_ms")?;
        ensure(timing.has_phases(), false, "has_phases")
    }

    #[test]
    fn with_phases_creates_timing_with_breakdown() -> TestResult {
        let phases = vec![
            TimingPhase::new("gather", Duration::from_millis(10)),
            TimingPhase::new("render", Duration::from_millis(5)),
        ];
        let timing = DiagnosticTiming::with_phases(Duration::from_millis(15), phases);
        ensure(timing.elapsed_ms, 15.0, "elapsed_ms")?;
        ensure(timing.has_phases(), true, "has_phases")?;
        ensure(timing.phases.len(), 2, "phase count")
    }

    #[test]
    fn timing_capture_measures_elapsed() -> TestResult {
        let capture = TimingCapture::start();
        thread::sleep(Duration::from_millis(5));
        let timing = capture.finish();

        // Should be at least 5ms
        if timing.elapsed_ms < 4.0 {
            return Err(format!(
                "elapsed too short: expected >= 4ms, got {}ms",
                timing.elapsed_ms
            ));
        }
        ensure(timing.has_phases(), false, "no phases when none marked")
    }

    #[test]
    fn timing_capture_records_phases() -> TestResult {
        let mut capture = TimingCapture::start();
        thread::sleep(Duration::from_millis(2));
        capture.mark("phase1");
        thread::sleep(Duration::from_millis(2));
        capture.mark("phase2");
        let timing = capture.finish();

        ensure(timing.has_phases(), true, "has phases")?;
        // At least 2 phases: phase1, phase2, and potentially finish
        if timing.phases.len() < 2 {
            return Err(format!(
                "expected at least 2 phases, got {}",
                timing.phases.len()
            ));
        }
        ensure(timing.phases[0].name, "phase1", "first phase name")?;
        ensure(timing.phases[1].name, "phase2", "second phase name")
    }

    #[test]
    fn timing_phase_converts_duration_to_ms() -> TestResult {
        let phase = TimingPhase::new("test", Duration::from_micros(1500));
        ensure(phase.name, "test", "phase name")?;
        ensure(phase.duration_ms, 1.5, "duration in ms")
    }

    #[test]
    fn first_mark_labels_segment_from_start() -> TestResult {
        // The first mark must label the segment from `start` (not the segment
        // that comes after the mark). This pins the documented semantics so a
        // future refactor can't silently flip the meaning of `mark`.
        let mut capture = TimingCapture::start();
        thread::sleep(Duration::from_millis(3));
        capture.mark("gather");
        let timing = capture.finish();

        ensure(timing.phases[0].name, "gather", "first phase name")?;
        if timing.phases[0].duration_ms < 2.0 {
            return Err(format!(
                "first phase should cover the pre-mark sleep, got {}ms",
                timing.phases[0].duration_ms
            ));
        }
        Ok(())
    }
}
