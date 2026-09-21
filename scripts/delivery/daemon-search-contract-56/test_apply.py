import importlib.util
import json
import unittest
from pathlib import Path

ROOT=Path(__file__).resolve().parents[3]
spec=importlib.util.spec_from_file_location('delivery',ROOT/'scripts/delivery/daemon-search-contract-56/apply.py')
m=importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
SHARED=(ROOT/'src/core/search_result_document.rs').read_text()
CONTRACT=(ROOT/'src/daemon/search_result_contract.rs').read_text()


def search_fixture():
    required=m.shared_fields(SHARED,'REQUIRED')
    optional=m.shared_fields(SHARED,'OPTIONAL')
    fields=list(required)+['calibrationId']
    initial=''.join(f'                    "{field}": sample_{variant},\n' for field,variant in ((k,(required|optional)[k]) for k in fields))
    inserts=''.join(f'                    obj_map.insert("{field}".to_string(), value.clone());\n' for field in optional if field != 'calibrationId')
    return ('    fn data_json_with_advisory_session_inner(\n' +
            '        let results: Vec<serde_json::Value> = visible_results\n' +
            '            .iter()\n            .map(|hit| {\n' +
            '                let mut obj = serde_json::json!({\n' + initial + '                });\n' +
            '                if let Some(obj_map) = obj.as_object_mut() {\n' + inserts + '                }\n' +
            '                obj\n            })\n            .collect();\n' +
            '        let consensus_conflicts = unchanged();\n')


def server_fixture():
    function=CONTRACT[CONTRACT.index('pub(super) fn validate_canonical_search_result('):]
    function=function[:function.index('\n#[cfg(test)]')]
    function=function.replace('pub(super) fn','fn',1).replace(m.ID_CHECK,'',1)
    lists=''
    for name in ('REQUIRED','OPTIONAL'):
        items=[x for x in m.shared_fields(SHARED,name) if x!='calibrationId']
        lists += '    const '+name+': &[&str] = &[\n'+''.join('        "'+x+'",\n' for x in items)+'    ];\n'
    function=function.replace('    let context =',lists+'    let context =',1)
    return 'use cass_prefetch_worker::CassPrefetchWorker;\n\n'+function+'\nfn dispatch_pack_search(\n'

CLI='''const DAEMON_SEARCH_FALLBACK_CODE: &str = "daemon_search_fallback";
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DaemonSearchFallbackReason {
    SearchRoundTripFailed,
    SearchResponseDrift,
}
impl DaemonSearchFallbackReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SearchResponseDrift => "search response drift",
            _ => "search round-trip failed",
        }
    }
}
fn daemon_search_fallback_degradation(reason: DaemonSearchFallbackReason) -> SearchDegradation {
    SearchDegradation { message: format!("Warm daemon search unavailable ({}); used canonical in-process search.", reason.as_str()) }
}
fn caller() {
    result.map_err(|_| DaemonSearchFallbackReason::SearchResponseDrift)
}
'''

class DeliveryTests(unittest.TestCase):
    def test_stale_workflows_cannot_publish_a_partial_integration(self):
        for version in (None, "1", "unexpected"):
            environment = {"GITHUB_ACTIONS": "true"}
            if version is not None:
                environment["EE_DAEMON_SEARCH_CONTRACT_DELIVERY"] = version
            with self.assertRaisesRegex(RuntimeError, "outdated workflow"):
                m.validate_delivery_environment(environment)
        m.validate_delivery_environment({
            "GITHUB_ACTIONS": "true", "EE_DAEMON_SEARCH_CONTRACT_DELIVERY": "2",
        })
        m.validate_delivery_environment({})
    def test_shared_vocabulary_has_no_duplicate_fields(self):
        r=m.shared_fields(SHARED,'REQUIRED');o=m.shared_fields(SHARED,'OPTIONAL')
        self.assertEqual(len(r),10);self.assertEqual(len(o),21)
        self.assertFalse(set(r)&set(o));self.assertIn('calibrationId',o)
    def test_producer_all_keys_become_typed_and_is_idempotent(self):
        result=m.updated_search(search_fixture(),SHARED)
        self.assertEqual(result.count('Field::'),31)
        self.assertNotIn('obj.as_object_mut()',result)
        self.assertEqual(result,m.updated_search(result,SHARED))
    def test_unreviewed_producer_field_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError,'unreviewed optional'):
            m.updated_search(search_fixture().replace('"memoryId".to_string()','"futureField".to_string()'),SHARED)
    def test_missing_producer_field_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError,'optional emission changed'):
            m.updated_search(search_fixture().replace('                    obj_map.insert("memoryId".to_string(), value.clone());\n',''),SHARED)
    def test_existing_validator_semantics_are_preserved(self):
        updated=m.updated_server(server_fixture(),CONTRACT,SHARED)
        self.assertNotIn('fn validate_canonical_search_result(',updated)
        self.assertIn(m.LINK.strip(),updated)
        self.assertEqual(updated,m.updated_server(updated,CONTRACT,SHARED))
    def test_concurrent_validator_change_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError,'validator logic changed'):
            m.updated_server(server_fixture().replace('if !value.is_finite()', 'if !value.is_finite() || value < 0.0'),CONTRACT,SHARED)
    def test_cli_diagnostic_is_lossless_and_idempotent(self):
        updated=m.updated_cli(CLI)
        self.assertIn('SearchResponseDrift(String)',updated)
        self.assertIn('.map_err(DaemonSearchFallbackReason::SearchResponseDrift)',updated)
        self.assertNotIn('reason.as_str()',updated)
        self.assertIn('const fn as_str(&self)',updated)
        self.assertEqual(updated,m.updated_cli(updated))
    def test_all_lossy_cli_mappings_are_repaired(self):
        second='fn second() { result.map_err(|_|\n    DaemonSearchFallbackReason::SearchResponseDrift) }\n'
        updated=m.updated_cli(CLI+second)
        self.assertEqual(updated.count('.map_err(DaemonSearchFallbackReason::SearchResponseDrift)'),2)
    def test_missing_cli_mapping_aborts(self):
        with self.assertRaisesRegex(RuntimeError,'missing reviewed lossy'):
            m.updated_cli(CLI.replace('.map_err(|_| DaemonSearchFallbackReason::SearchResponseDrift)',''))
    def test_module_link_is_idempotent(self):
        result=m.updated_core('pub mod search;\npub mod sentinel;\n')
        self.assertEqual(result,m.updated_core(result))
    def test_partial_cli_wiring_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError,'partial fallback'):
            m.updated_cli('mod daemon_search_diagnostics;\n'+CLI)
    def test_schema_only_changes_calibration_contract(self):
        raw='''{
  "$defs": {
    "searchDocument": {
      "required": ["scoreKind", "calibrationId", "scoreInterval"],
      "properties": {
        "scoreKind": {"type": "string"},
        "scoreInterval": {"type": "array"}
      }
    }
  }
}'''
        result=m.updated_schema(raw)
        schema=json.loads(result)['$defs']['searchDocument']
        self.assertNotIn('calibrationId',schema['required'])
        self.assertEqual(schema['properties']['calibrationId']['type'],['string','null'])
        self.assertEqual(result,m.updated_schema(result))

if __name__=='__main__':unittest.main(verbosity=2)
