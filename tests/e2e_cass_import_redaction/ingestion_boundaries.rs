//! Full public CLI loop with a fixture CASS subprocess and real storage/search.
//! Both cases require the exact admitted evidence in search AND a native pack;
//! merely hiding the dangerous input or returning no results is not success.

use super::*;

#[test]
fn label_fused_credential_is_scrubbed_from_import_search_and_native_pack() -> TestResult {
    let token = format!("{}{}", "ghp_", "Q".repeat(36));
    let content = format!(
        "quartzcanaryproof compilation succeeded and release verification passed. label-{token}"
    );
    exercise("label-fused", &content, &[token, "ghp_QQQQQ".to_owned()])
}

#[test]
fn credential_crossing_excerpt_boundary_never_leaves_a_searchable_fragment() -> TestResult {
    let prefix = "sk-proj-";
    let token = format!("{prefix}{}", "Q".repeat(40));
    let lead = "quartzcanaryproof compilation succeeded and release verification passed. ";
    let token_start = 65_536 - prefix.len() - 5;
    let content = format!("{lead}{}{token}", " ".repeat(token_start - lead.len()));
    // A fixed point of the old excerpt-before-screen order: only five key
    // suffix bytes survive, which is too short for the provider detector.
    let old_excerpt = &content[..65_536];
    ensure(
        !ee::policy::screen_external_text_for_ingestion(old_excerpt).redacted,
        "fixture must demonstrate the old truncate-before-screen gap",
    )?;
    exercise("cutoff", &content, &[token, "sk-proj-QQQQQ".to_owned()])
}

fn exercise(case: &str, content: &str, forbidden: &[String]) -> TestResult {
    let root = unique_artifact_dir(&format!("external-ingestion-{case}"))?;
    let workspace = root.join("workspace");
    let fake_bin = root.join("bin");
    fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
    fs::create_dir_all(&fake_bin).map_err(|e| e.to_string())?;
    set_executable_dir_permissions(&fake_bin)?;
    let session = workspace.join("transcript.jsonl");
    fs::write(&session, format!("{content}\n")).map_err(|e| e.to_string())?;
    let view = root.join("view.jsonl");
    write_fake_view_jsonl(&view, content)?;
    let cass = fake_bin.join("cass");
    write_fake_cass_binary(&cass)?;
    let path = path_with_fake_cass(&fake_bin)?;
    let workspace = workspace.canonicalize().map_err(|e| e.to_string())?;
    let session = session.canonicalize().map_err(|e| e.to_string())?;
    let database = root.join("memory.db");
    let index = root.join("index");
    precreate_workspace_database(&database, &workspace)?;
    let ws = workspace.to_string_lossy().into_owned();
    let db_arg = database.to_string_lossy().into_owned();
    let index_arg = index.to_string_lossy().into_owned();
    let session_arg = session.to_string_lossy().into_owned();
    let cass_arg = cass.to_string_lossy().into_owned();
    let view_arg = view.to_string_lossy().into_owned();

    let first = run_import_once(&ws, &db_arg, &session_arg, &cass_arg, &view_arg, &path)?;
    ensure_success(&first, "first import")?;
    ensure_equal(
        &report_count(&first, "sessionsImported")?,
        &1,
        "one imported session",
    )?;
    ensure_equal(
        &report_count(&first, "spansImported")?,
        &1,
        "one imported span",
    )?;
    ensure_output_omits_raw_values("import response", &first.to_string(), forbidden)?;
    let second = run_import_once(&ws, &db_arg, &session_arg, &cass_arg, &view_arg, &path)?;
    ensure_success(&second, "retry import")?;
    ensure_equal(
        &report_count(&second, "sessionsImported")?,
        &0,
        "no duplicate session",
    )?;
    ensure_equal(
        &report_count(&second, "sessionsSkipped")?,
        &1,
        "exact skipped session",
    )?;
    ensure_equal(
        &report_count(&second, "spansImported")?,
        &0,
        "no duplicate evidence",
    )?;
    ensure_output_omits_raw_values("retry response", &second.to_string(), forbidden)?;

    let db = DbConnection::open(DatabaseConfig::read_only_file(database.clone()))
        .map_err(|e| e.to_string())?;
    let ws_id = stable_workspace_id(&ws);
    let sessions = db.list_sessions(&ws_id).map_err(|e| e.to_string())?;
    ensure_equal(&sessions.len(), &1, "exact durable session count")?;
    let spans = db
        .list_evidence_spans_for_session(&sessions[0].id)
        .map_err(|e| e.to_string())?;
    ensure_equal(&spans.len(), &1, "exact durable evidence count")?;
    let span = &spans[0];
    ensure(span.excerpt.len() <= 65_536, "excerpt byte bound")?;
    ensure(
        span.excerpt.contains("quartzcanaryproof"),
        "retain useful task evidence",
    )?;
    ensure(
        span.excerpt.contains("[REDACTED:"),
        "retain a durable screening marker",
    )?;
    ensure_equal(
        &span.secret_redaction_status.as_str(),
        &"redacted",
        "redaction posture",
    )?;
    let class = if case == "cutoff" {
        "openai_api_key"
    } else {
        "github_token"
    };
    let classes: serde_json::Value =
        serde_json::from_str(&span.redaction_classes_json).map_err(|e| e.to_string())?;
    ensure_equal(
        &classes,
        &serde_json::json!([class]),
        "exact retained provider class",
    )?;
    ensure_equal(
        &span.content_hash,
        &format!("blake3:{}", blake3::hash(span.excerpt.as_bytes()).to_hex()),
        "canonical screened hash",
    )?;
    ensure(
        db.get_search_admitted_evidence_span(&span.id, &ws_id)
            .map_err(|e| e.to_string())?
            .is_some(),
        "safe evidence remains live-admitted",
    )?;
    ensure_equal(
        &db.count_table_rows("memories").map_err(|e| e.to_string())?,
        &0,
        "no synthetic memories",
    )?;
    let audits = db
        .list_audit_by_action("cass.evidence.redacted", None)
        .map_err(|e| e.to_string())?;
    ensure_equal(&audits.len(), &1, "one redaction audit despite retry")?;
    ensure_equal(
        &audits[0].target_id.as_deref(),
        &Some(span.id.as_str()),
        "audit binds exact evidence",
    )?;
    ensure_output_omits_raw_values("stored evidence", &span.excerpt, forbidden)?;
    let id = span.id.clone();
    let safe_content = span.excerpt.clone();
    db.close().map_err(|e| e.to_string())?;

    let rebuilt = run_ee_json(
        &ws,
        [
            "index",
            "rebuild",
            "--database",
            db_arg.as_str(),
            "--index-dir",
            index_arg.as_str(),
        ],
        "rebuild",
    )?;
    ensure_success(&rebuilt, "rebuild")?;
    let search = run_ee_json(
        &ws,
        [
            "search",
            "quartzcanaryproof",
            "--database",
            db_arg.as_str(),
            "--index-dir",
            index_arg.as_str(),
            "--source-mode",
            "lexical_only",
            "--limit",
            "10",
        ],
        "search",
    )?;
    ensure_success(&search, "search")?;
    let results = json_field(&search, &["data", "results"], "search results")?
        .as_array()
        .ok_or("results must be an array")?;
    ensure_equal(
        &results
            .iter()
            .filter(|r| r["docId"].as_str() == Some(id.as_str()))
            .count(),
        &1,
        "exact evidence is searchable",
    )?;
    ensure_output_omits_raw_values("search response", &search.to_string(), forbidden)?;

    let pack = run_ee_json(
        &ws,
        [
            "pack",
            "quartzcanaryproof",
            "--read-only",
            "--database",
            db_arg.as_str(),
            "--index-dir",
            index_arg.as_str(),
            "--source-mode",
            "lexical_only",
            "--max-tokens",
            "20000",
        ],
        "native evidence pack",
    )?;
    ensure_success(&pack, "native evidence pack")?;
    // The public response unifies typed evidence and memories in items[].
    let items = json_field(&pack, &["data", "pack", "items"], "pack items")?
        .as_array()
        .ok_or("pack items must be an array")?;
    ensure_equal(
        &items.len(),
        &1,
        "one exact native evidence item, not an empty pack",
    )?;
    ensure_equal(
        &items[0]["entityKind"],
        &serde_json::json!("evidence_span"),
        "native evidence kind",
    )?;
    ensure_equal(
        &items[0]["evidenceSpanId"],
        &serde_json::json!(id),
        "native evidence identity",
    )?;
    ensure_equal(
        &items[0]["content"],
        &serde_json::json!(safe_content),
        "pack uses canonical screened body",
    )?;
    ensure(
        items[0].get("memoryId").is_none(),
        "no fabricated memory identity",
    )?;
    ensure_output_omits_raw_values("pack response", &pack.to_string(), forbidden)?;
    ensure_database_files_omit_raw_values(&database, forbidden)
}
