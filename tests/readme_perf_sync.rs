use serde_json::Value;

const README: &str = include_str!("../README.md");
const PERF_BASELINE: &str = include_str!("../benches/baselines/perf_v0_2.json");
const HARDWARE_CLASSES: &str = include_str!("../benches/baselines/hardware_classes.toml");
const PERF_SYNC_GOLDEN: &str = include_str!("golden/perf_table_sync.snap");

const CANONICAL_ROWS: [(&str, &str); 10] = [
    ("ee_remember", "`ee remember` (single record)"),
    ("ee_search", "`ee search \"<q>\"` (hybrid)"),
    (
        "ee_context",
        "`ee context \"<task>\"` (markdown, 4k tokens)",
    ),
    ("ee_why", "`ee why <id>`"),
    ("ee_workspace_init", "`ee init --workspace <dir>` (clean)"),
    ("ee_audit_query", "`ee audit timeline --limit 1000`"),
    ("ee_import_cass", "`ee import cass --limit 50` (cold)"),
    (
        "ee_graph_pagerank",
        "`ee graph centrality-refresh` (PageRank, 5k links)",
    ),
    ("ee_index_rebuild", "`ee index rebuild` (full)"),
    (
        "ee_concurrent_writes",
        "4 concurrent audited memory writers",
    ),
];

fn trace_perf_table_sync(phase: &'static str, elapsed_ms: u64, degraded_codes: &[&str]) {
    tracing::info!(
        workspace_id = "repo",
        request_id = "readme_perf_sync_contract",
        bead_id = option_env!("EE_TRACE_BEAD_ID").unwrap_or("bd-3usjw.8"),
        surface = "perf_table_sync",
        phase,
        elapsed_ms,
        degraded_codes = ?degraded_codes,
        "perf table sync contract checkpoint"
    );
}

fn perf_block() -> Result<&'static str, String> {
    trace_perf_table_sync("input", 0, &[]);
    let start = README
        .find("<!-- perf:begin")
        .ok_or_else(|| "README missing perf begin marker".to_owned())?;
    let end_marker = "<!-- perf:end -->";
    let relative_end = README[start..]
        .find(end_marker)
        .ok_or_else(|| "README missing perf end marker".to_owned())?;
    let end = start + relative_end + end_marker.len();
    trace_perf_table_sync("response", 0, &[]);
    Ok(&README[start..end])
}

#[test]
fn readme_perf_block_is_bound_to_canonical_hardware_manifest() -> Result<(), String> {
    assert_eq!(
        README.matches("<!-- perf:begin").count(),
        1,
        "README must contain exactly one perf begin marker"
    );
    assert_eq!(
        README.matches("<!-- perf:end -->").count(),
        1,
        "README must contain exactly one perf end marker"
    );

    let block = perf_block()?;
    assert!(
        block.starts_with(
            "<!-- perf:begin hardware-class=mac-m3-pro baseline=benches/baselines/perf_v0_2.json -->",
        ),
        "README perf begin marker must pin mac-m3-pro perf_v0_2"
    );
    assert!(
        HARDWARE_CLASSES.contains("[classes.mac-m3-pro]"),
        "hardware class manifest missing mac-m3-pro"
    );
    assert!(
        HARDWARE_CLASSES.contains("file = \"benches/baselines/perf_v0_2.json\""),
        "hardware class manifest does not pin perf_v0_2 baseline"
    );
    Ok(())
}

#[test]
fn readme_perf_rows_follow_canonical_order_and_shape() -> Result<(), String> {
    let block = perf_block()?;
    let rows = block
        .lines()
        .filter(|line| line.starts_with("| ") && !line.starts_with("| Operation |"))
        .filter(|line| !line.starts_with("|---"))
        .collect::<Vec<_>>();

    assert_eq!(
        rows.len(),
        CANONICAL_ROWS.len(),
        "README perf table row count"
    );

    for ((_, expected_label), row) in CANONICAL_ROWS.iter().zip(rows.iter()) {
        let cells = row
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        assert_eq!(cells.len(), 4, "README perf row must have 4 cells: {row}");
        assert_eq!(cells[0], *expected_label, "README perf row order mismatch");
        assert_eq!(
            cells[1], "`mac-m3-pro`",
            "README perf row has wrong hardware class: {row}"
        );
        assert!(
            cells[2].ends_with(" ms") || cells[2].ends_with(" s"),
            "README perf p50 must include units: {row}"
        );
        assert!(
            cells[3].ends_with(" ms") || cells[3].ends_with(" s"),
            "README perf p99 must include units: {row}"
        );
    }

    assert!(
        block.contains("Last synced: ") && block.contains(" from sha256:"),
        "README perf block missing sync footer"
    );
    Ok(())
}

#[test]
fn baseline_contains_every_readme_synced_operation() -> Result<(), String> {
    let baseline: Value = serde_json::from_str(PERF_BASELINE)
        .map_err(|error| format!("invalid baseline: {error}"))?;
    assert_eq!(
        baseline.get("schema").and_then(Value::as_str),
        Some("ee.perf.baseline.v1"),
        "perf_v0_2 must use ee.perf.baseline.v1 schema"
    );

    let operations = baseline
        .get("operations")
        .and_then(Value::as_object)
        .ok_or_else(|| "perf_v0_2 missing operations object".to_owned())?;

    for (key, _) in CANONICAL_ROWS {
        let operation = operations
            .get(key)
            .ok_or_else(|| format!("perf_v0_2 missing operation {key}"))?;
        assert!(
            operation.get("p50_ms").and_then(Value::as_f64).is_some(),
            "operation {key} missing p50_ms"
        );
        assert!(
            operation.get("p99_ms").and_then(Value::as_f64).is_some(),
            "operation {key} missing p99_ms"
        );
    }
    Ok(())
}

#[test]
fn readme_perf_block_matches_golden_snapshot() -> Result<(), String> {
    let expected = PERF_SYNC_GOLDEN
        .strip_suffix('\n')
        .unwrap_or(PERF_SYNC_GOLDEN);
    assert_eq!(perf_block()?, expected, "README perf block drifted");
    Ok(())
}
