import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("integration", Path(__file__).with_name("integrate.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

# Use the actual topology that the earlier synthetic fixture missed: cfg(test)
# belongs to the constant, daemon_fallback takes &str, and pack code still has
# unit-variant returns and an Option::ok_or in addition to fallible decoding.
CLI = '''#[cfg(test)]
const DAEMON_SEARCH_FALLBACK_CODE: &str = "daemon_search_fallback";
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonSearchFallbackReason {
    SearchResponseDrift,
}
impl DaemonSearchFallbackReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SearchResponseDrift => "search response drift",
        }
    }
}
fn daemon_search_fallback_degradation(reason: DaemonSearchFallbackReason) -> SearchDegradation {
    SearchDegradation::daemon_fallback(reason.as_str())
}
fn search() {
    result.map_err(|_| DaemonSearchFallbackReason::SearchResponseDrift)?;
    if missing_performance { return Err(DaemonSearchFallbackReason::SearchResponseDrift); }
}
fn pack() {
    value.ok_or(DaemonSearchFallbackReason::SearchResponseDrift)?;
    decoder.map_err(|_|\n        DaemonSearchFallbackReason::SearchResponseDrift)?;
    if wrong_request { return Err(DaemonSearchFallbackReason::SearchResponseDrift); }
}
'''
GLOBAL = '''        fn count(&self, table: &str) -> u64 {
            self.destination
                .count_table_rows(table)
                .expect("durable count")
        }
assert_eq!(count, before_audits + u64::try_from(successes).unwrap());
assert_eq!(count, before_jobs + u64::try_from(successes).unwrap());
'''


class CompletionTests(unittest.TestCase):
    def test_diagnostics_are_not_test_only(self):
        result = m.updated_cli(CLI)
        self.assertIn('#[cfg(test)]\nconst DAEMON_SEARCH_FALLBACK_CODE', result)
        self.assertNotIn('#[cfg(test)]\nmod daemon_search_diagnostics;', result)
        self.assertIn('mod daemon_search_diagnostics;\n\n#[derive(Clone, Debug, Eq, PartialEq)]', result)

    def test_all_error_types_use_the_display_adapter(self):
        result = m.updated_cli(CLI)
        self.assertEqual(result.count('.map_err(DaemonSearchFallbackReason::search_response_drift)'), 2)
        self.assertNotIn('.map_err(|_|', result)
        self.assertIn('SearchResponseValidationError(String)', result)

    def test_other_enum_implementations_are_untouched(self):
        other = "impl OtherReason {\n    const fn as_str(self) -> &'static str { \"other\" }\n}\n"
        result = m.updated_cli(other + CLI)
        self.assertTrue(result.startswith(other))
        self.assertIn("impl DaemonSearchFallbackReason {\n    const fn as_str(&self)", result)

    def test_unit_variant_callers_remain_valid(self):
        result = m.updated_cli(CLI)
        self.assertIn('    SearchResponseDrift,', result)
        self.assertEqual(result.count('Err(DaemonSearchFallbackReason::SearchResponseDrift)'), 2)
        self.assertIn('.ok_or(DaemonSearchFallbackReason::SearchResponseDrift)', result)

    def test_degradation_receives_a_borrowed_diagnostic_string(self):
        result = m.updated_cli(CLI)
        self.assertIn('SearchDegradation::daemon_fallback(&reason.to_string())', result)

    def test_completion_is_idempotent(self):
        result = m.updated_cli(CLI)
        self.assertEqual(result, m.updated_cli(result))
        formatted = result.replace('Self::SearchResponseValidationError(_) => "search response drift",',
                                   'Self::SearchResponseValidationError(_) => {\n "search response drift"\n },')
        self.assertEqual(formatted, m.updated_cli(formatted))

    def test_test_only_or_partial_wiring_is_rejected(self):
        result = m.updated_cli(CLI)
        with self.assertRaisesRegex(RuntimeError, 'outside tests'):
            m.updated_cli(result.replace('mod daemon_search_diagnostics;', '#[cfg(test)]\nmod daemon_search_diagnostics;'))
        with self.assertRaisesRegex(RuntimeError, 'partial fallback'):
            m.updated_cli(result.replace('&reason.to_string()', 'reason.as_str()'))

    def test_missing_lossy_mapping_is_not_silently_accepted(self):
        with self.assertRaisesRegex(RuntimeError, 'missing reviewed lossy'):
            m.updated_cli(CLI[:CLI.index('fn search()')])

    def test_test_count_fixes_are_checked_and_idempotent(self):
        result = m.updated_global_promotion(GLOBAL)
        self.assertIn('.try_into()\n                .expect("non-negative row count")', result)
        self.assertEqual(result.count('i64::try_from(successes)'), 2)
        self.assertEqual(result, m.updated_global_promotion(result))

    def test_concurrent_signed_count_repair_is_preserved_verbatim(self):
        signed = GLOBAL.replace("-> u64", "-> i64").replace("u64::try_from", "i64::try_from")
        self.assertEqual(signed, m.updated_global_promotion(signed))

    def test_concurrent_test_fixture_changes_are_not_overwritten(self):
        with self.assertRaisesRegex(RuntimeError, 'test fixture count changed'):
            m.updated_global_promotion(GLOBAL.replace('"durable count"', '"changed count"'))


if __name__ == '__main__':
    unittest.main(verbosity=2)
