use serde_json::Value;

const README: &str = include_str!("../README.md");
const PERF_BASELINE: &str = include_str!("../benches/baselines/perf_v0_2.json");
const HARDWARE_CLASSES: &str = include_str!("../benches/baselines/hardware_classes.toml");

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

fn perf_block() -> Result<&'static str, String> {
    let start = README
        .find("<!-- perf:begin")
        .ok_or_else(|| "README missing perf begin marker".to_owned())?;
    let end_marker = "<!-- perf:end -->";
    let relative_end = README[start..]
        .find(end_marker)
        .ok_or_else(|| "README missing perf end marker".to_owned())?;
    let end = start + relative_end + end_marker.len();
    Ok(&README[start..end])
}

#[test]
fn readme_perf_block_is_bound_to_canonical_hardware_manifest() -> Result<(), String> {
    if README.matches("<!-- perf:begin").count() != 1 {
        return Err("README must contain exactly one perf begin marker".to_owned());
    }
    if README.matches("<!-- perf:end -->").count() != 1 {
        return Err("README must contain exactly one perf end marker".to_owned());
    }

    let block = perf_block()?;
    if !block.starts_with(
        "<!-- perf:begin hardware-class=mac-m3-pro baseline=benches/baselines/perf_v0_2.json -->",
    ) {
        return Err("README perf begin marker must pin mac-m3-pro perf_v0_2".to_owned());
    }
    if !HARDWARE_CLASSES.contains("[classes.mac-m3-pro]") {
        return Err("hardware class manifest missing mac-m3-pro".to_owned());
    }
    if !HARDWARE_CLASSES.contains("file = \"benches/baselines/perf_v0_2.json\"") {
        return Err("hardware class manifest does not pin perf_v0_2 baseline".to_owned());
    }
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

    if rows.len() != CANONICAL_ROWS.len() {
        return Err(format!(
            "README perf table has {} rows; expected {}",
            rows.len(),
            CANONICAL_ROWS.len()
        ));
    }

    for ((_, expected_label), row) in CANONICAL_ROWS.iter().zip(rows.iter()) {
        let cells = row
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        if cells.len() != 4 {
            return Err(format!("README perf row must have 4 cells: {row}"));
        }
        if cells[0] != *expected_label {
            return Err(format!(
                "README perf row order mismatch: expected `{expected_label}`, got `{}`",
                cells[0]
            ));
        }
        if cells[1] != "`mac-m3-pro`" {
            return Err(format!("README perf row has wrong hardware class: {row}"));
        }
        if !(cells[2].ends_with(" ms") || cells[2].ends_with(" s")) {
            return Err(format!("README perf p50 must include units: {row}"));
        }
        if !(cells[3].ends_with(" ms") || cells[3].ends_with(" s")) {
            return Err(format!("README perf p99 must include units: {row}"));
        }
    }

    if !block.contains("Last synced: ") || !block.contains(" from sha256:") {
        return Err("README perf block missing sync footer".to_owned());
    }
    Ok(())
}

#[test]
fn baseline_contains_every_readme_synced_operation() -> Result<(), String> {
    let baseline: Value = serde_json::from_str(PERF_BASELINE)
        .map_err(|error| format!("invalid baseline: {error}"))?;
    if baseline.get("schema").and_then(Value::as_str) != Some("ee.perf.baseline.v1") {
        return Err("perf_v0_2 must use ee.perf.baseline.v1 schema".to_owned());
    }

    let operations = baseline
        .get("operations")
        .and_then(Value::as_object)
        .ok_or_else(|| "perf_v0_2 missing operations object".to_owned())?;

    for (key, _) in CANONICAL_ROWS {
        let operation = operations
            .get(key)
            .ok_or_else(|| format!("perf_v0_2 missing operation {key}"))?;
        if operation.get("p50_ms").and_then(Value::as_f64).is_none() {
            return Err(format!("operation {key} missing p50_ms"));
        }
        if operation.get("p99_ms").and_then(Value::as_f64).is_none() {
            return Err(format!("operation {key} missing p99_ms"));
        }
    }
    Ok(())
}
