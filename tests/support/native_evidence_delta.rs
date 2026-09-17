//! Public EE import -> pack -> persisted baseline -> native-evidence delta.
//!
//! Reuse the existing no_mocks_e2e harness. CASS is the same explicit contract
//! subprocess stub; EE, FrankenSQLite, retrieval, ledger verification and delta
//! emission are real. No fallback schema or empty positive control can pass.

use super::*;

fn native_ids(pack: &JsonValue) -> Result<Vec<String>, String> {
    json_array(pack, "/data/pack/items", "native delta full pack")?
        .iter()
        .filter(|item| item["entityKind"] == "evidence_span")
        .map(|item| {
            item["evidenceSpanId"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "native full-pack item omitted evidenceSpanId".to_owned())
        })
        .collect()
}

#[test]
fn imported_evidence_survives_add_noop_and_remove_deltas() -> TestResult {
    let scenario_id = "native_evidence_context_delta";
    let log_dir = unique_log_dir(scenario_id)?;
    let artifacts = log_dir.join("artifacts");
    let events = log_dir.join("commands.jsonl");
    let workspace = log_dir.join("workspace");
    let home = log_dir.join("home");
    let codex_home = log_dir.join("codex-home");
    let cass_data = log_dir.join("cass-data");
    for directory in [&artifacts, &workspace, &home, &codex_home, &cass_data] {
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    }
    let session = write_codex_cass_fixture_session(&codex_home, &workspace)?;
    let stub_dir = tempfile::Builder::new()
        .prefix("ee-delta-cass-stub-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let stub = write_stub_cass_binary(
        stub_dir.path(),
        &log_dir.join("cass-payloads"),
        &session,
        &workspace,
    )?;
    let envs = vec![
        ("HOME", home.as_os_str().to_owned()),
        ("CODEX_HOME", codex_home.as_os_str().to_owned()),
        ("CASS_DATA_DIR", cass_data.as_os_str().to_owned()),
        ("CASS_IGNORE_SOURCES_CONFIG", OsString::from("1")),
        ("EE_CASS_BINARY", stub.binary.as_os_str().to_owned()),
        ("PATH", path_with_binary_parent(&stub.binary)?),
        ("CODING_AGENT_SEARCH_NO_UPDATE_PROMPT", OsString::from("1")),
        ("NO_COLOR", OsString::from("1")),
    ];
    let run = |name: &'static str, args: Vec<String>, schema: &'static str| {
        let mut full_args = vec![
            "--workspace".to_owned(),
            workspace.display().to_string(),
            "--json".to_owned(),
        ];
        full_args.extend(args);
        run_step_with_env(
            scenario_id,
            &events,
            &artifacts,
            &workspace,
            StepSpec {
                name,
                args: full_args,
                expected_exit_code: 0,
                expected_schema: schema,
                expect_clean_stderr: true,
            },
            &envs,
        )
        .map(|(_, value)| value)
    };
    run("01_init", vec!["init".to_owned()], "ee.response.v2")?;
    let imported = run(
        "02_import",
        vec!["import".into(), "cass".into(), "--limit".into(), "5".into()],
        "ee.response.v2",
    )?;
    ensure_equal(
        &imported["data"]["sessionsImported"],
        &json!(1),
        "imported session count",
    )?;

    let memory_query = "Q7R9V3 widget purple latch";
    let remembered = run(
        "03_remember_memory_baseline",
        vec![
            "remember".into(),
            "--level".into(),
            "semantic".into(),
            "--kind".into(),
            "note".into(),
            "--source".into(),
            "file://tests/no_mocks_e2e.rs#L1".into(),
            "Only the Q7R9V3 widget uses the purple latch.".into(),
        ],
        "ee.response.v2",
    )?;
    let memory_id = string_at(&remembered, "/data/memory_id", "manual baseline memory")?;
    let pack_args = |query: &str, since: Option<&str>| {
        let mut args = vec![
            "pack".to_owned(),
            query.to_owned(),
            "--source-mode".into(),
            "lexical_only".into(),
            "--max-tokens".into(),
            "1200".into(),
        ];
        if let Some(hash) = since {
            args.extend([
                "--since".into(),
                hash.to_owned(),
                "--read-only".into(),
                "--max-delta-bytes".into(),
                "1000000".into(),
            ]);
        }
        args
    };
    let memory_pack = run(
        "04_memory_pack",
        pack_args(memory_query, None),
        "ee.response.v2",
    )?;
    ensure(
        native_ids(&memory_pack)?.is_empty(),
        "baseline must contain no native evidence",
    )?;
    ensure(
        json_array(&memory_pack, "/data/pack/items", "memory baseline")?
            .iter()
            .any(|item| item["memoryId"].as_str() == Some(memory_id.as_str())),
        "baseline must include the exact manual memory, not an empty positive control",
    )?;
    let memory_hash = string_at(&memory_pack, "/data/pack/hash", "memory baseline hash")?;
    let evidence_query = "x65f imported CASS evidence remains durable and searchable";
    let evidence_pack = run(
        "05_evidence_pack",
        pack_args(evidence_query, None),
        "ee.response.v2",
    )?;
    let expected_ids = native_ids(&evidence_pack)?;
    ensure(
        !expected_ids.is_empty(),
        "native evidence positive control must not be empty",
    )?;
    let evidence_hash = string_at(&evidence_pack, "/data/pack/hash", "native baseline hash")?;
    ensure(
        memory_hash != evidence_hash,
        "different baseline contents must have different hashes",
    )?;

    let added = run(
        "06_delta_add_native_evidence",
        pack_args(evidence_query, Some(&memory_hash)),
        "ee.context.delta.v2",
    )?;
    ensure_equal(
        &added["data"]["priorPackHash"],
        &json!(memory_hash),
        "addition baseline hash",
    )?;
    ensure_equal(
        &added["data"]["newPackHash"],
        &json!(evidence_hash),
        "addition new hash",
    )?;
    ensure_equal(
        &added["data"]["serverDecision"]["computedFromServerVerifiedPackRecord"],
        &json!(true),
        "addition baseline is centrally verified",
    )?;
    let additions = json_array(&added, "/data/items/added", "native additions")?;
    let added_ids = additions
        .iter()
        .filter(|item| item.pointer("/fields/entityKind") == Some(&json!("evidence_span")))
        .map(|item| {
            item["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or("native addition omitted id".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    ensure_equal(
        &added_ids,
        &expected_ids,
        "native additions preserve exact IDs and order",
    )?;
    let full_items = json_array(&evidence_pack, "/data/pack/items", "native full items")?;
    for id in &expected_ids {
        let full = full_items
            .iter()
            .find(|item| item["evidenceSpanId"].as_str() == Some(id.as_str()))
            .ok_or("expected native full item disappeared")?;
        let item = additions
            .iter()
            .find(|item| item["id"].as_str() == Some(id.as_str()))
            .ok_or("expected native addition disappeared")?;
        ensure_equal(
            &item["fields"]["entityRevision"],
            &full["entityRevision"],
            "native revision",
        )?;
        ensure_equal(&item["fields"]["rank"], &full["rank"], "native rank")?;
        ensure_equal(
            &item["fields"]["estimatedTokens"],
            &full["estimatedTokens"],
            "native token cost",
        )?;
        ensure_equal(
            &item["fields"]["trustClass"],
            &json!("cass_evidence"),
            "native trust is not elevated",
        )?;
        ensure(
            item["fields"].get("memoryId").is_none(),
            "native evidence has no synthetic memory ID",
        )?;
    }

    let unchanged = run(
        "07_delta_native_noop",
        pack_args(evidence_query, Some(&evidence_hash)),
        "ee.context.delta.v2",
    )?;
    ensure_equal(
        &unchanged["data"]["newPackHash"],
        &json!(evidence_hash),
        "unchanged native hash",
    )?;
    for pointer in [
        "/data/items/added",
        "/data/items/removed",
        "/data/items/modified",
    ] {
        ensure(
            json_array(&unchanged, pointer, "native no-op")?.is_empty(),
            format!("unchanged native evidence must not generate {pointer}"),
        )?;
    }
    let removed = run(
        "08_delta_remove_native_evidence",
        pack_args(memory_query, Some(&evidence_hash)),
        "ee.context.delta.v2",
    )?;
    let removed_ids = json_array(&removed, "/data/items/removed", "native removals")?
        .iter()
        .filter_map(JsonValue::as_str)
        .filter(|id| id.starts_with("ev_"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    ensure_equal(
        &removed_ids,
        &expected_ids.into_iter().collect::<BTreeSet<_>>(),
        "native removals preserve every exact prior evidence ID",
    )?;
    for value in [&added, &unchanged, &removed] {
        ensure(
            value["data"]["serverDecision"]
                .get("fallbackReason")
                .is_none(),
            "native delta must not disguise full-pack fallback as success",
        )?;
        let output = value.to_string();
        ensure(
            !output.contains("x65f transcript control "),
            "delta excludes transcript-control canaries",
        )?;
        ensure(
            !output.contains(DENIED_CASS_PRIVATE_PATH)
                && !output.contains(DENIED_CASS_SECRET_PROBE),
            "delta does not reveal private evidence fixtures",
        )?;
    }
    Ok(())
}
