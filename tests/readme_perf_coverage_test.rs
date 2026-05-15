use std::fs;
use std::path::Path;

use serde_json::Value;

const README: &str = include_str!("../README.md");
const PERF_BASELINE_PATH: &str = "benches/baselines/perf_v0_2.json";
const BUDGETS_PATH: &str = "benches/budgets.toml";

type TestResult<T = ()> = Result<T, String>;

#[derive(Debug)]
struct PerfClaim {
    row_index: usize,
    operation: String,
    p50_ms: f64,
    p99_ms: f64,
    advisory: bool,
}

#[derive(Clone, Copy, Debug)]
struct Coverage {
    operation_key: &'static str,
    bench_name: &'static str,
    bench_path: &'static str,
}

fn remove_html_comments(input: &str) -> String {
    let mut output = input.to_owned();
    while let Some(start) = output.find("<!--") {
        let Some(relative_end) = output[start + 4..].find("-->") else {
            break;
        };
        let end = start + 4 + relative_end + 3;
        output.replace_range(start..end, "");
    }
    output
}

fn strip_cell_markup(input: &str) -> String {
    remove_html_comments(input)
        .replace('`', "")
        .replace("**", "")
        .trim()
        .to_owned()
}

fn parse_latency_ms(input: &str) -> TestResult<f64> {
    let plain = strip_cell_markup(input)
        .replace(',', "")
        .to_ascii_lowercase();
    let trimmed = plain.trim();
    if let Some(value) = trimmed.strip_suffix("ms") {
        return value
            .trim()
            .parse::<f64>()
            .map_err(|error| format!("invalid millisecond latency `{input}`: {error}"));
    }
    if let Some(value) = trimmed.strip_suffix('s') {
        return value
            .trim()
            .parse::<f64>()
            .map(|seconds| seconds * 1000.0)
            .map_err(|error| format!("invalid second latency `{input}`: {error}"));
    }
    Err(format!(
        "README performance latency `{input}` must use ms or s units"
    ))
}

fn parse_perf_table() -> TestResult<Vec<PerfClaim>> {
    let mut in_performance_section = false;
    let mut in_table = false;
    let mut claims = Vec::new();

    for line in README.lines() {
        let trimmed = line.trim();
        if trimmed == "## Performance" {
            in_performance_section = true;
            continue;
        }
        if in_performance_section && trimmed.starts_with("## ") {
            break;
        }
        if !in_performance_section {
            continue;
        }
        if trimmed.starts_with("| Operation |") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if !trimmed.starts_with('|') {
            break;
        }
        if trimmed.contains("|---") {
            continue;
        }

        let cells = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        if cells.len() < 3 {
            return Err(format!("malformed README performance row: `{line}`"));
        }
        let p50_cell = if cells.len() >= 4 { cells[2] } else { cells[1] };
        let p99_cell = if cells.len() >= 4 { cells[3] } else { cells[2] };

        let row_index = claims.len();
        claims.push(PerfClaim {
            row_index,
            operation: strip_cell_markup(cells[0]),
            p50_ms: parse_latency_ms(p50_cell)?,
            p99_ms: parse_latency_ms(p99_cell)?,
            advisory: line.contains("(advisory, not gated)") || line.contains("<!-- advisory:"),
        });
    }

    if claims.is_empty() {
        return Err("README performance table was not found".to_owned());
    }

    Ok(claims)
}

fn coverage_for_operation(operation: &str) -> Option<Coverage> {
    let normalized = operation.to_ascii_lowercase();
    if normalized.contains("ee remember") {
        return Some(Coverage {
            operation_key: "ee_remember",
            bench_name: "remember",
            bench_path: "benches/remember.rs",
        });
    }
    if normalized.contains("ee search") {
        return Some(Coverage {
            operation_key: "ee_search",
            bench_name: "search",
            bench_path: "benches/search.rs",
        });
    }
    if normalized.contains("ee context") {
        return Some(Coverage {
            operation_key: "ee_context",
            bench_name: "context",
            bench_path: "benches/context.rs",
        });
    }
    if normalized.contains("ee why") {
        return Some(Coverage {
            operation_key: "ee_why",
            bench_name: "why",
            bench_path: "benches/why.rs",
        });
    }
    if normalized.contains("ee init") {
        return Some(Coverage {
            operation_key: "ee_workspace_init",
            bench_name: "workspace_init",
            bench_path: "benches/workspace_init.rs",
        });
    }
    if normalized.contains("ee audit timeline") {
        return Some(Coverage {
            operation_key: "ee_audit_query",
            bench_name: "audit_query",
            bench_path: "benches/audit_query.rs",
        });
    }
    if normalized.contains("ee import cass") {
        return Some(Coverage {
            operation_key: "ee_import_cass",
            bench_name: "import_cass",
            bench_path: "benches/import_cass.rs",
        });
    }
    if normalized.contains("ee graph centrality-refresh") {
        return Some(Coverage {
            operation_key: "ee_graph_pagerank",
            bench_name: "graph_pagerank",
            bench_path: "benches/graph_pagerank.rs",
        });
    }
    if normalized.contains("ee index rebuild") {
        return Some(Coverage {
            operation_key: "ee_index_rebuild",
            bench_name: "index_rebuild",
            bench_path: "benches/index_rebuild.rs",
        });
    }
    if normalized.contains("concurrent audited memory writers") {
        return Some(Coverage {
            operation_key: "ee_concurrent_writes",
            bench_name: "concurrent_writes",
            bench_path: "benches/concurrent_writes.rs",
        });
    }
    None
}

fn assert_close(actual: f64, expected: f64, label: &str) -> TestResult {
    if (actual - expected).abs() > 0.001 {
        return Err(format!(
            "{label} mismatch: README has {actual:.3}ms, baseline has {expected:.3}ms"
        ));
    }
    Ok(())
}

fn assert_bench_declares_budget_and_thresholds(path: &str, source: &str) -> TestResult {
    let has_p50_budget = source.contains("BUDGET_P50_MS")
        || source.contains("BUDGET_TARGET_P50_MS")
        || source.contains("TARGET_P50_MS");
    let has_p99_budget = source.contains("BUDGET_P99_MS")
        || source.contains("BUDGET_TARGET_P99_MS")
        || source.contains("HARD_CEILING_MS");
    let has_regression_threshold =
        source.contains("REGRESSION_THRESHOLD") || source.contains("REGRESSION_THRESHOLD_P50_PCT");

    if !has_p50_budget {
        return Err(format!("{path} does not declare a p50 budget constant"));
    }
    if !has_p99_budget {
        return Err(format!(
            "{path} does not declare a p99 budget or ceiling constant"
        ));
    }
    if !has_regression_threshold {
        return Err(format!("{path} does not declare a regression threshold"));
    }
    Ok(())
}

fn unstable_expected(operation_key: &str) -> bool {
    matches!(
        operation_key,
        "ee_import_cass" | "ee_graph_pagerank" | "ee_index_rebuild" | "ee_concurrent_writes"
    )
}

#[test]
fn every_readme_perf_row_has_bench_or_advisory_marker() -> TestResult {
    let claims = parse_perf_table()?;
    let cargo_toml = fs::read_to_string("Cargo.toml")
        .map_err(|error| format!("failed to read Cargo.toml: {error}"))?;
    let bench_script = fs::read_to_string("scripts/bench.sh")
        .map_err(|error| format!("failed to read scripts/bench.sh: {error}"))?;
    let budgets = fs::read_to_string(BUDGETS_PATH)
        .map_err(|error| format!("failed to read {BUDGETS_PATH}: {error}"))?;
    let baseline = fs::read_to_string(PERF_BASELINE_PATH)
        .map_err(|error| format!("failed to read {PERF_BASELINE_PATH}: {error}"))?;
    let baseline: Value = serde_json::from_str(&baseline)
        .map_err(|error| format!("invalid perf baseline JSON: {error}"))?;

    for claim in &claims {
        if claim.advisory {
            continue;
        }

        let coverage = coverage_for_operation(&claim.operation).ok_or_else(|| {
            format!(
                "README perf row {} `{}` has no known bench mapping and no advisory marker",
                claim.row_index, claim.operation
            )
        })?;

        if !Path::new(coverage.bench_path).is_file() {
            return Err(format!(
                "README perf row `{}` maps to missing {}",
                claim.operation, coverage.bench_path
            ));
        }
        if !cargo_toml.contains(&format!("name = \"{}\"", coverage.bench_name)) {
            return Err(format!(
                "Cargo.toml does not register bench `{}` for README row `{}`",
                coverage.bench_name, claim.operation
            ));
        }
        if !bench_script.contains(coverage.bench_name) {
            return Err(format!(
                "scripts/bench.sh does not run bench `{}` for README row `{}`",
                coverage.bench_name, claim.operation
            ));
        }
        if !budgets.contains(&format!("[operations.{}]", coverage.operation_key)) {
            return Err(format!(
                "{BUDGETS_PATH} missing [operations.{}] for README row `{}`",
                coverage.operation_key, claim.operation
            ));
        }

        let bench_source = fs::read_to_string(coverage.bench_path)
            .map_err(|error| format!("failed to read {}: {error}", coverage.bench_path))?;
        assert_bench_declares_budget_and_thresholds(coverage.bench_path, &bench_source)?;

        let operation = baseline
            .pointer(&format!("/operations/{}", coverage.operation_key))
            .ok_or_else(|| {
                format!(
                    "{PERF_BASELINE_PATH} missing operations.{} for README row `{}`",
                    coverage.operation_key, claim.operation
                )
            })?;
        let baseline_p50 = operation
            .get("p50_ms")
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("baseline {} missing p50_ms", coverage.operation_key))?;
        let baseline_p99 = operation
            .get("p99_ms")
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("baseline {} missing p99_ms", coverage.operation_key))?;
        assert_close(
            claim.p50_ms,
            baseline_p50,
            &format!("{} p50", claim.operation),
        )?;
        assert_close(
            claim.p99_ms,
            baseline_p99,
            &format!("{} p99", claim.operation),
        )?;

        for field in ["tolerance_pct_p50", "tolerance_pct_p99", "unstable"] {
            if operation.get(field).is_none() {
                return Err(format!(
                    "baseline {} missing required field `{field}`",
                    coverage.operation_key
                ));
            }
        }

        let unstable = operation
            .get("unstable")
            .and_then(Value::as_bool)
            .ok_or_else(|| format!("baseline {} unstable must be bool", coverage.operation_key))?;
        if unstable != unstable_expected(coverage.operation_key) {
            return Err(format!(
                "baseline {} unstable={unstable}; expected {}",
                coverage.operation_key,
                unstable_expected(coverage.operation_key)
            ));
        }
    }

    Ok(())
}

#[test]
fn readme_perf_table_contains_the_expected_current_rows() -> TestResult {
    let claims = parse_perf_table()?;
    let operations = claims
        .iter()
        .map(|claim| claim.operation.as_str())
        .collect::<Vec<_>>();

    for expected in [
        "ee remember",
        "ee search",
        "ee context",
        "ee why",
        "ee init",
        "ee audit timeline",
        "ee import cass",
        "ee graph centrality-refresh",
        "ee index rebuild",
        "concurrent audited memory writers",
    ] {
        if !operations
            .iter()
            .any(|operation| operation.contains(expected))
        {
            return Err(format!("README performance table missing `{expected}`"));
        }
    }

    Ok(())
}

#[test]
fn perf_advisory_files_have_marker_and_review_date() -> TestResult {
    let advisory_dir = Path::new("benches/advisories");
    if !advisory_dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(advisory_dir)
        .map_err(|error| format!("failed to read benches/advisories: {error}"))?
    {
        let entry = entry.map_err(|error| format!("failed to read advisory dir entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| format!("invalid advisory filename: {}", path.display()))?;
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        if !source.contains(&format!("<!-- advisory:{name} -->")) {
            return Err(format!(
                "{} missing advisory marker <!-- advisory:{name} -->",
                path.display()
            ));
        }
        if !source.contains("Last reviewed:") {
            return Err(format!("{} missing Last reviewed line", path.display()));
        }
    }

    Ok(())
}
