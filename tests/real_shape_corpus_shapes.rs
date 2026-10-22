//! Every record in the committed real-shape CASS corpus matches the envelope
//! table it claims to mirror (bd-reality-core-convergence-1azkt.43, deliverable 2).
//!
//! The corpus exists because every earlier pass proved the CASS and Learn loops
//! on clean, tiny, synthetic fixtures, and "clean fixtures validate nothing"
//! describes exactly that. The risk this guard exists to stop is the corpus
//! quietly drifting BACK into clean shapes -- someone regenerates, a renderer
//! changes, and the measuring stick stops measuring the thing it was built for
//! while every test over it still passes.
//!
//! THE RULE IS MIRRORED FROM `scripts/real_shape_corpus/generate.py::validate`,
//! deliberately, not re-derived: the three tables below are transcribed from
//! CLAUDE_REQUIRED, CODEX_REQUIRED and CLAUDE_BLOCKS at generate.py:377-388. If
//! the renderer's tables move, this test must move with them, and the test will
//! red first -- which is the intended ordering.
//!
//! THE TRAP, recorded on the bead before this file existed and encoded here so
//! nobody re-learns it: `type` is the DISPATCH KEY for the envelope table, so a
//! mutation that strips `type` short-circuits with "unknown record type" and the
//! missing-envelope-fields arm never runs. Stripping `type` therefore reddens
//! without testing the envelope check at all. The controls below exercise both
//! arms separately for that reason.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

type TestResult = Result<(), String>;

/// Transcribed from generate.py:377 CLAUDE_REQUIRED.
fn claude_required() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        (
            "user",
            vec![
                "parentUuid",
                "isSidechain",
                "userType",
                "cwd",
                "sessionId",
                "version",
                "gitBranch",
                "type",
                "message",
                "uuid",
                "timestamp",
            ],
        ),
        (
            "assistant",
            vec![
                "parentUuid",
                "isSidechain",
                "userType",
                "cwd",
                "sessionId",
                "version",
                "gitBranch",
                "message",
                "requestId",
                "type",
                "uuid",
                "timestamp",
            ],
        ),
        ("summary", vec!["type", "summary", "leafUuid"]),
    ])
}

/// Transcribed from generate.py:384 CODEX_REQUIRED.
fn codex_required() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        ("session_meta", vec!["timestamp", "type", "payload"]),
        ("turn_context", vec!["timestamp", "type", "payload"]),
        ("response_item", vec!["timestamp", "type", "payload"]),
    ])
}

/// Transcribed from generate.py:387 CLAUDE_BLOCKS.
fn claude_blocks() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        ("text", vec!["text"]),
        ("thinking", vec!["thinking", "signature"]),
        ("tool_use", vec!["id", "name", "input"]),
        ("tool_result", vec!["tool_use_id", "content"]),
    ])
}

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("real_shape_cass")
}

/// Problems for ONE record. Extracted so the controls can exercise each arm on
/// a synthetic record instead of mutating the committed corpus: a mutation
/// control is observed once and then nobody sees it again, while this re-runs.
fn record_problems(agent: &str, rec: &Value, whence: &str) -> Vec<String> {
    let table = if agent == "claude_code" {
        claude_required()
    } else {
        codex_required()
    };
    let kind = rec.get("type").and_then(Value::as_str);
    let Some(kind) = kind.filter(|k| table.contains_key(k)) else {
        return vec![format!("{whence}: unknown record type {kind:?}")];
    };

    let mut problems = Vec::new();
    let missing: Vec<&str> = table[kind]
        .iter()
        .copied()
        .filter(|key| rec.get(key).is_none())
        .collect();
    if !missing.is_empty() {
        problems.push(format!("{whence}: missing envelope fields {missing:?}"));
    }

    if agent == "claude_code" && (kind == "user" || kind == "assistant") {
        let content = rec.get("message").and_then(|m| m.get("content"));
        if kind == "assistant" && !content.is_some_and(Value::is_array) {
            problems.push(format!("{whence}: assistant content must be a block list"));
        }
        if let Some(blocks) = content.and_then(Value::as_array) {
            let shapes = claude_blocks();
            for block in blocks {
                let btype = block.get("type").and_then(Value::as_str);
                let ok = btype.is_some_and(|t| {
                    shapes
                        .get(t)
                        .is_some_and(|need| need.iter().all(|k| block.get(k).is_some()))
                });
                if !ok {
                    problems.push(format!("{whence}: malformed {btype:?} block"));
                }
            }
        }
    }
    problems
}

struct Corpus {
    problems: Vec<String>,
    records: usize,
    sessions: usize,
}

fn validate_corpus(root: &Path) -> Result<Corpus, String> {
    let manifest_path = root.join("manifest.json");
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest: Value =
        serde_json::from_str(&raw).map_err(|error| format!("parse manifest.json: {error}"))?;

    let sessions = manifest
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or_else(|| "manifest.json has no sessions array".to_owned())?;

    let mut problems = Vec::new();
    let mut records = 0usize;
    for session in sessions {
        let rel = session
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "a manifest session has no path".to_owned())?;
        let agent = session.get("agent").and_then(Value::as_str).unwrap_or("");
        let text = fs::read_to_string(root.join(rel))
            .map_err(|error| format!("read session {rel}: {error}"))?;
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            records += 1;
            let whence = format!("{rel}:{}", index + 1);
            match serde_json::from_str::<Value>(line) {
                Ok(rec) => problems.extend(record_problems(agent, &rec, &whence)),
                Err(_) => problems.push(format!("{whence}: not JSON")),
            }
        }
    }

    // Label locators must resolve, which is what makes judgments.json usable.
    if let Some(labels) = manifest.get("labels").and_then(Value::as_object) {
        for (label, locator) in labels {
            let Some(rel) = locator.get("path").and_then(Value::as_str) else {
                problems.push(format!("label {label}: locator has no path"));
                continue;
            };
            if !root.join(rel).exists() {
                problems.push(format!("label {label}: missing session file"));
            }
        }
    }

    Ok(Corpus {
        problems,
        records,
        sessions: sessions.len(),
    })
}

#[test]
fn real_shape_corpus_records_match_the_envelope_table() -> TestResult {
    let root = corpus_root();
    let corpus = validate_corpus(&root)?;

    // EMPTY-WORLD FLOOR. A corpus that failed to resolve would report zero
    // problems over zero records, and this test would pass by finding no work.
    // The committed tree is 308 sessions and 4009 records; these floors are far
    // below that and only fire when the walk resolved nothing.
    if corpus.sessions < 12 {
        return Err(format!(
            "walked only {} session(s); deliverable 1 requires at least 12, so the \
             corpus did not resolve and an empty pass here would mean nothing",
            corpus.sessions
        ));
    }
    if corpus.records < 3000 {
        return Err(format!(
            "walked only {} record(s) across {} session(s); the corpus is specified at \
             roughly 3-6k records, so this is a resolution failure rather than a clean run",
            corpus.records, corpus.sessions
        ));
    }

    if !corpus.problems.is_empty() {
        return Err(format!(
            "{} record(s) in tests/fixtures/real_shape_cass do not match the envelope \
             table transcribed from generate.py. The corpus has drifted toward the clean \
             shapes it exists to replace, or the renderer's tables moved without this \
             test. Regenerate with `python3 scripts/real_shape_corpus/generate.py --out \
             tests/fixtures/real_shape_cass --target-records 4000 --digest`:\n{}",
            corpus.problems.len(),
            corpus
                .problems
                .iter()
                .take(10)
                .map(|problem| format!("  {problem}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    Ok(())
}

/// Both arms of the envelope check, on synthetic records, because a check
/// nobody has seen fail is decoration -- and because the two arms are reached
/// through different paths.
#[test]
fn the_envelope_check_fires_on_each_arm_separately() {
    let good = serde_json::json!({
        "parentUuid": null, "isSidechain": false, "userType": "external",
        "cwd": "/work/ledger", "sessionId": "s", "version": "2.0.14",
        "gitBranch": "main", "type": "user", "uuid": "u", "timestamp": "t",
        "message": {"content": "hello"}
    });
    assert!(
        record_problems("claude_code", &good, "synthetic:1").is_empty(),
        "a well-formed user record must raise nothing"
    );

    // ARM 1: a required field removed, `type` KEPT so dispatch still succeeds.
    let mut stripped = good.clone();
    stripped.as_object_mut().unwrap().remove("isSidechain");
    let problems = record_problems("claude_code", &stripped, "synthetic:1");
    assert!(
        problems
            .iter()
            .any(|p| p.contains("missing envelope fields")),
        "removing isSidechain must reach the envelope arm; got {problems:?}"
    );

    // ARM 2: `type` removed. This is the trap -- it reddens via the DISPATCH
    // arm, and the envelope arm is never evaluated, so a control that only
    // strips `type` proves less than it appears to.
    let mut no_type = good.clone();
    no_type.as_object_mut().unwrap().remove("type");
    let problems = record_problems("claude_code", &no_type, "synthetic:1");
    assert!(
        problems.iter().any(|p| p.contains("unknown record type")),
        "removing type must be rejected; got {problems:?}"
    );
    assert!(
        !problems
            .iter()
            .any(|p| p.contains("missing envelope fields")),
        "stripping type must SHORT-CIRCUIT before the envelope arm -- if this ever \
         reports both, the dispatch and envelope arms have been merged and the trap \
         recorded in this file's header no longer holds: {problems:?}"
    );

    // ARM 3: a malformed content block.
    let mut bad_block = good.clone();
    bad_block["message"] = serde_json::json!({"content": [{"type": "tool_use", "id": "x"}]});
    let problems = record_problems("claude_code", &bad_block, "synthetic:1");
    assert!(
        problems.iter().any(|p| p.contains("malformed")),
        "a tool_use block missing name and input must be flagged; got {problems:?}"
    );

    // ARM 4: an assistant record whose content is not a list.
    let mut scalar_content = good.clone();
    scalar_content["type"] = serde_json::json!("assistant");
    scalar_content
        .as_object_mut()
        .unwrap()
        .insert("requestId".to_owned(), serde_json::json!("r"));
    let problems = record_problems("claude_code", &scalar_content, "synthetic:1");
    assert!(
        problems
            .iter()
            .any(|p| p.contains("assistant content must be a block list")),
        "assistant content must be required to be a block list; got {problems:?}"
    );
}

/// Generator determinism, which the bead's TESTS list names and which was
/// previously only a measurement (bd-reality-core-convergence-1azkt.43).
///
/// Regenerates the corpus into a TEMP directory with the committed parameters
/// and compares the generator's own tree digest to the digest recorded in
/// tests/fixtures/real_shape_cass/SHAPES.md. Two things fail here that nothing
/// else catches: a renderer change that alters bytes while still satisfying the
/// envelope table, and a regeneration committed with a different seed or
/// target-records than the manifest claims.
///
/// It never touches the committed fixture -- the output goes to a tempdir, and
/// only the printed digest is compared.
///
/// SKIPS RATHER THAN FAILS when python3 or the generator is absent, because a
/// tarball without scripts/ cannot answer this and a gate that assumes the
/// generator is present fails for a reason unrelated to determinism.
#[test]
fn the_generator_is_deterministic_for_the_committed_parameters() -> TestResult {
    const RECORDED_DIGEST: &str =
        "7bcedd76fcc6c3e9913fa5d30efe54ffccd43eeea72299d56ce581ff65753faa";

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let generator = repo.join("scripts").join("real_shape_corpus").join("generate.py");
    if !generator.exists() {
        eprintln!("skip: {} is absent", generator.display());
        return Ok(());
    }

    let manifest_raw = fs::read_to_string(corpus_root().join("manifest.json"))
        .map_err(|error| format!("read manifest.json: {error}"))?;
    let manifest: Value =
        serde_json::from_str(&manifest_raw).map_err(|error| format!("parse manifest: {error}"))?;
    let seed = manifest.get("seed").and_then(Value::as_i64).unwrap_or(-1);
    let target = manifest
        .get("target_records")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    // The workspace comes from the manifest, never from the generator's own
    // default: it is baked into every session PATH, and the paths are part of
    // the tree digest. Relying on the default would make this test red for a
    // changed default rather than for lost determinism.
    let workspace = manifest
        .get("workspace")
        .and_then(Value::as_str)
        .unwrap_or("");
    if seed < 0 || target < 0 || workspace.is_empty() {
        return Err(format!(
            "manifest.json must record seed, target_records and workspace so this \
             test can regenerate with the SAME parameters; got seed={seed} \
             target={target} workspace={workspace:?}"
        ));
    }

    // THE OUTPUT DIRECTORY MUST BE EMPTY, and this is not fussiness.
    // `tree_digest` (generate.py:429) walks the WHOLE out tree and hashes every
    // file it finds, while `generate` (:282) never clears that tree. So one
    // stray file -- a leftover from an earlier run, an editor backup, a
    // .DS_Store -- changes the digest and reds this test for a reason that has
    // nothing to do with determinism. A pid-stamped name alone does not get
    // there: pids are reused and nothing here removes the directory afterward.
    //
    // For the same reason the recorded digest CANNOT be reproduced by hashing
    // tests/fixtures/real_shape_cass/ in place: that directory also carries
    // SHAPES.md, which generate.py does not write. The digest is a property of
    // a freshly generated tree, which is what this test builds.
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let out = std::env::temp_dir()
        .join(format!("ee-real-shape-determinism-{}-{unique}", std::process::id()));
    if out.exists() {
        return Err(format!(
            "{} already exists; this test needs a tree it alone wrote, because \
             tree_digest hashes every file present",
            out.display()
        ));
    }
    let output = std::process::Command::new("python3")
        .arg(&generator)
        .arg("--out")
        .arg(&out)
        .arg("--workspace")
        .arg(workspace)
        .arg("--seed")
        .arg(seed.to_string())
        .arg("--target-records")
        .arg(target.to_string())
        .arg("--digest")
        .current_dir(&repo)
        .output();
    let output = match output {
        Ok(value) => value,
        Err(error) => {
            // THIS IS NOT A SKIP, deliberately. `generate.py` exists -- checked
            // above -- so this repo intends determinism to be checkable here,
            // and python3 is not optional for it: 64 tracked .py files and
            // several verify.sh stages already require it.
            //
            // A skip here would be INVISIBLE in the one place it matters. cargo
            // captures a passing test's stderr, so `eprintln!("skip: ...")`
            // never reaches an operator reading `test result: ok. N passed`, and
            // a run that verified nothing would be indistinguishable from a run
            // that verified the digest. That is the defect this whole file
            // exists to prevent, so the test refuses to report success it did
            // not earn. The generator-absent case above is a real skip; this is
            // a broken environment.
            return Err(format!(
                "generate.py is present but python3 could not be run ({error}), \
                 so determinism was NOT verified and this test will not report \
                 success. Install python3, or remove the generator if this corpus \
                 is no longer reproducible."
            ));
        }
    };
    if !output.status.success() {
        return Err(format!(
            "generate.py exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or_default()
        ));
    }

    let summary: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "generate.py --digest must print a JSON summary: {error}; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })?;
    let digest = summary.get("digest").and_then(Value::as_str).unwrap_or("");
    if digest != RECORDED_DIGEST {
        return Err(format!(
            "regenerating with seed={seed} target_records={target} produced digest\n  \
             {digest}\nbut tests/fixtures/real_shape_cass/SHAPES.md records\n  \
             {RECORDED_DIGEST}\nEither the generator is no longer deterministic, or the \
             committed corpus was produced with different parameters than the manifest \
             claims. Regenerate deliberately and update SHAPES.md in the same commit."
        ));
    }

    // And the regenerated tree must agree with the committed one on the counts
    // the manifest publishes, so a digest match cannot come from both sides
    // being equally wrong.
    let regenerated_manifest = fs::read_to_string(out.join("manifest.json"))
        .map_err(|error| format!("read regenerated manifest: {error}"))?;
    let regenerated: Value = serde_json::from_str(&regenerated_manifest)
        .map_err(|error| format!("parse regenerated manifest: {error}"))?;
    for key in ["total_records", "seed", "target_records", "workspace"] {
        if regenerated.get(key) != manifest.get(key) {
            return Err(format!(
                "regenerated manifest disagrees on {key}: {:?} vs committed {:?}",
                regenerated.get(key),
                manifest.get(key)
            ));
        }
    }
    Ok(())
}

/// The manifest pins the upstream formats as DATA, so a reader can assert on
/// them rather than trusting prose. If an upstream format moves, this is where
/// the corpus stops claiming to mirror the old one.
#[test]
fn manifest_pins_the_upstream_shape_versions() -> TestResult {
    let root = corpus_root();
    let raw = fs::read_to_string(root.join("manifest.json"))
        .map_err(|error| format!("read manifest.json: {error}"))?;
    let manifest: Value =
        serde_json::from_str(&raw).map_err(|error| format!("parse manifest.json: {error}"))?;

    for key in ["schema", "claude_code_version", "codex_cli_version"] {
        let value = manifest.get(key).and_then(Value::as_str).unwrap_or("");
        if value.is_empty() {
            return Err(format!(
                "manifest.json is missing `{key}`; without it the corpus does not say \
                 which upstream shape it mirrors and SHAPES.md's claim is unverifiable"
            ));
        }
    }
    let schema = manifest.get("schema").and_then(Value::as_str).unwrap_or("");
    if schema != "ee.real_shape_cass.manifest.v1" {
        return Err(format!(
            "manifest schema is {schema:?}; this test was written against \
             ee.real_shape_cass.manifest.v1 and must be updated deliberately"
        ));
    }
    Ok(())
}
