use std::{fs, path::PathBuf};

fn repo_file(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", path.display());
    })
}

fn assert_contains(haystack: &str, needle: &str) {
    assert!(
        haystack.contains(needle),
        "expected release provenance contract to contain {needle:?}"
    );
}

#[test]
fn release_workflow_generates_slsa_v1_provenance_per_target() {
    let workflow = repo_file(".github/workflows/release.yml");

    for needle in [
        "name: Generate SLSA provenance",
        "\"_type\": \"https://in-toto.io/Statement/v1\"",
        "\"predicateType\": \"https://slsa.dev/provenance/v1\"",
        "\"subject\"",
        "\"sha256\": actual_sha256",
        "\"target\": target",
        "\"cargoCommand\": cargo_command",
        "\"features\": \"default\"",
        "\"runnerOs\"",
        "\"runnerImage\"",
        "\"sourceRepository\": repo",
        "\"sourceCommit\": os.environ[\"EE_RELEASE_SHA\"]",
        "\"workflowRef\": os.environ[\"EE_RELEASE_WORKFLOW_REF\"]",
        "git+{server_url}/{repo}",
        "\"uri\": \"file://Cargo.lock\"",
        "\"blake3\": os.environ[\"LOCK_BLAKE3\"]",
        "b3sum ../Cargo.lock",
    ] {
        assert_contains(&workflow, needle);
    }
}

#[test]
fn release_workflow_uploads_and_validates_provenance_assets() {
    let workflow = repo_file(".github/workflows/release.yml");

    for needle in [
        "dist/ee-${{ matrix.target }}.provenance.json",
        "dist/ee-${{ matrix.target }}.provenance.json.sigstore.json",
        "Verify Sigstore bundles and provenance",
        "provenance=\"${artifact%.tar.xz}.provenance.json\"",
        "Missing provenance",
        "Missing provenance Sigstore bundle",
        "subject digest does not match artifact",
        "missing source commit dependency",
        "missing Cargo.lock blake3 dependency",
    ] {
        assert_contains(&workflow, needle);
    }
}

#[test]
fn provenance_docs_and_audit_surface_are_registered() {
    let readme = repo_file("README.md");
    let checklist = repo_file("PUBLISH_CHECKLIST.md");
    let audit = repo_file("scripts/audit_install_pipeline.sh");

    // bd-oxajr. Five of this test's eleven needles pinned a posture the
    // project does not have, and the docs were deliberately corrected away
    // from it: PUBLISH_CHECKLIST.md now states that releases "do not carry
    // Sigstore bundles or SLSA attestations", that "`--require-provenance`
    // remains unsatisfied", and -- in as many words -- "Do not describe
    // unsigned metadata as signed provenance". Demanding the checklist say
    // "Signed release provenance ready" asked it to assert something false,
    // so the repair that makes this green by editing the docs is the wrong
    // one: it re-introduces the overclaim those commits removed.
    //
    // The true posture is CAPABILITY PRESENT, RELEASES UNSIGNED. The
    // installer takes `--require-provenance` and verifies a Sigstore bundle
    // when one is published; no published release carries one yet. The
    // needles below pin that, and they are not weaker than what they
    // replace: the checklist block now also pins the anti-overclaim warning,
    // so re-describing unsigned metadata as signed reds this test.
    //
    // DELIBERATE TRIPWIRE: `--require-provenance` remains unsatisfied` is
    // pinned on purpose. The day releases are signed, that sentence must
    // change and this test must go red, forcing the docs, the audit surface
    // and these assertions to move together instead of drifting apart again.
    for needle in [
        "| Path | Status | Provenance | Tracking |",
        "`--require-provenance` for fail-closed signature and SLSA provenance",
        "SLSA provenance JSON and its Sigstore bundle",
    ] {
        assert_contains(&readme, needle);
    }

    for needle in [
        "do not carry Sigstore bundles or SLSA attestations",
        // Not a bare "unsatisfied": that word is generic enough to keep
        // passing for an unrelated reason if this paragraph is rewritten.
        // The sentence wraps after "remains", so this is the longest span
        // that pins the claim without pinning the line wrapping too.
        "`--require-provenance` remains",
        "Do not describe unsigned metadata as signed provenance.",
    ] {
        assert_contains(&checklist, needle);
    }

    for needle in [
        "slsa_provenance_present",
        "provenance_bundle_present",
        "release_verifies_provenance_before_publish",
        "unix_installer_supports_required_provenance",
    ] {
        assert_contains(&audit, needle);
    }
}

/// bd-reality-core-convergence-1azkt.18: "Exact effective dependency identity
/// appears in ... post-release audit."
///
/// `dependency_resolution_inventory` in scripts/audit_install_pipeline.sh asks
/// crates.io, per crate, "is THIS version published and unyanked?". The
/// versions were a hand-maintained table with nothing holding them to the
/// resolved tree, and 30 of its 31 rows had drifted -- asupersync 0.4.10 against
/// a locked 0.5.0, every fsqlite crate at 0.3.16 against 0.4.0/0.4.1, and so on.
///
/// That is worse than an out-of-date comment. The probe was verifying that
/// SUPERSEDED versions were available on crates.io and reporting the release
/// ready, while the versions actually being shipped went unchecked. A green
/// that answers a question nobody asked.
///
/// ABSENCE FROM Cargo.lock IS NEVER LEGITIMATE, for any strategy. This comment
/// used to say the opposite -- "a row may legitimately be absent from Cargo.lock
/// when it is only pulled in under an optional feature, so absence is allowed
/// but only for rows whose strategy is conditional" -- and that premise is
/// false: Cargo resolves optional and feature-gated dependencies into the lock
/// regardless of which features are activated. The exemption it justified is
/// what let `fnx-convert` be probed for years while belonging to no build
/// (bd-wef5o).
///
/// ---
///
/// One inventory row judged against the resolved tree, or `None` when the row
/// is consistent with `Cargo.lock`.
///
/// EXTRACTED so the planted negative below can exercise the rule on synthetic
/// inputs. The gate itself reads the real repository, so without a seam there is
/// no way to show the check CAN fail -- and a check nobody has seen fail is
/// decoration.
///
/// THE ABSENT CASE NO LONGER DEPENDS ON `strategy` (bd-wef5o). It used to read
///
///     None if strategy != "must_be_published" => {}
///
/// so a row naming a crate that is in `Cargo.lock` under NO version at all was
/// silently accepted whenever its strategy was something else. `fnx-convert`
/// sat in exactly that gap: listed in AGENTS.md, probed by the inventory,
/// present in neither `Cargo.toml` nor `Cargo.lock`, referenced nowhere in
/// `src/`, and carrying `graph_feature_or_publish` rather than
/// `must_be_published`. Its crates.io probe answered a question about a crate
/// the build never pulls in, and contributed a pass to `dep_resolution_ready`,
/// which is an `all()` over the rows.
///
/// The exemption looked defensible -- "maybe it is feature-gated" -- and is
/// not: Cargo resolves OPTIONAL and feature-gated dependencies into
/// `Cargo.lock` regardless of which features are activated, so absence from the
/// lock means the crate is not a dependency under any feature. `fnx-cgse` is
/// the control and carries the same `graph_feature_or_publish` strategy while
/// resolving in the lock at 0.2.3.
fn inventory_row_problem(
    crate_name: &str,
    declared: &str,
    strategy: &str,
    locked: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    match locked.get(crate_name) {
        Some(resolved) if resolved == declared => None,
        Some(resolved) => Some(format!(
            "  {crate_name}: audit says {declared}, Cargo.lock resolves {resolved} \
             -- the probe would verify the wrong version on crates.io"
        )),
        None => Some(format!(
            "  {crate_name}: marked {strategy} but absent from Cargo.lock entirely \
             -- the probe verifies a crate this build does not pull in, under any \
             feature, and still contributes a pass to dep_resolution_ready"
        )),
    }
}

/// Planted negative for `inventory_row_problem`, covering all three verdicts
/// and specifically the one bd-wef5o found unguarded.
#[test]
fn inventory_row_problem_flags_a_row_absent_from_the_lock_whatever_its_strategy() {
    // Short bindings throughout: `inventory_row_problem(..).is_none()` is an
    // 88-character chain against rustfmt's 60-character chain_width, so the
    // inline form would be reformatted and red the Rustfmt Conformance stage.
    const COND: &str = "graph_feature_or_publish";
    const MUST: &str = "must_be_published";
    let locked: std::collections::BTreeMap<String, String> =
        [("fnx-cgse".to_owned(), "0.2.3".to_owned())]
            .into_iter()
            .collect();

    let matching = inventory_row_problem("fnx-cgse", "0.2.3", COND, &locked);
    assert!(matching.is_none(), "a row matching the lock is consistent");

    let drifted = inventory_row_problem("fnx-cgse", "0.2.2", COND, &locked);
    assert!(
        drifted.is_some(),
        "a row whose version differs from the lock must be flagged"
    );

    // THE REGRESSION ARM. Before bd-wef5o this returned None, because the
    // strategy was not `must_be_published`.
    let absent = inventory_row_problem("fnx-convert", "0.2.0", COND, &locked);
    assert!(
        absent.is_some(),
        "a row naming a crate absent from Cargo.lock must be flagged regardless \
         of strategy; this is the hole fnx-convert sat in"
    );
    let text = absent.unwrap_or_default();
    assert!(
        text.contains("absent from Cargo.lock"),
        "the message must say what is wrong, not merely that something is: {text}"
    );

    let absent_must = inventory_row_problem("fnx-convert", "0.2.0", MUST, &locked);
    assert!(
        absent_must.is_some(),
        "the must_be_published case that already worked must keep working"
    );
}

/// The gate: every inventory row in scripts/audit_install_pipeline.sh agrees
/// with Cargo.lock, and every lock crate in an enumerated family has a row.
/// The per-row rule and the history behind it live on
/// `inventory_row_problem` above.
#[test]
fn post_release_audit_inventory_matches_the_resolved_tree() {
    let audit = repo_file("scripts/audit_install_pipeline.sh");
    let lock = repo_file("Cargo.lock");

    let mut locked = std::collections::BTreeMap::new();
    let mut name: Option<String> = None;
    for line in lock.lines() {
        if let Some(value) = line.strip_prefix("name = \"") {
            name = value.strip_suffix('"').map(str::to_owned);
        } else if let Some(value) = line.strip_prefix("version = \"") {
            if let (Some(n), Some(v)) = (name.take(), value.strip_suffix('"')) {
                locked.insert(n, v.to_owned());
            }
        }
    }

    let rows: Vec<Vec<&str>> = audit
        .lines()
        .map(str::trim)
        .filter(|line| line.matches('|').count() == 4)
        .map(|line| line.split('|').collect::<Vec<_>>())
        .filter(|parts| {
            parts[1]
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();

    // Empty inventories and empty lockfiles both compare equal to everything.
    assert!(
        !locked.is_empty(),
        "parsed zero crates from Cargo.lock; the parser is broken, not the audit"
    );
    assert!(
        rows.len() > 10,
        "parsed {} inventory rows from audit_install_pipeline.sh; the heredoc \
         format changed and this test can no longer read it",
        rows.len()
    );

    let mut problems = Vec::new();
    for parts in &rows {
        let (crate_name, declared, strategy) = (parts[0], parts[1], parts[4]);
        if let Some(problem) = inventory_row_problem(crate_name, declared, strategy, &locked) {
            problems.push(problem);
        }
    }

    // Drift is only half of it. A crate the inventory never mentions is also
    // never probed, and `dep_resolution_ready` is an `all()` over the rows --
    // so an omission reads as readiness just as convincingly as a pass.
    //
    // The families here are not a guess: they are derived from the inventory's
    // own rows, and four of the seven (fnx 4/4, fsqlite 17/17, sqlmodel 2/2,
    // tru 1/1) already cover their family exactly. Exhaustive family coverage
    // is the existing intent; asupersync and frankensearch had holes.
    let families: std::collections::BTreeSet<&str> = rows
        .iter()
        .map(|parts| parts[0].split('-').next().unwrap_or(parts[0]))
        .collect();
    let listed: std::collections::BTreeSet<&str> = rows.iter().map(|parts| parts[0]).collect();

    let mut omitted = Vec::new();
    for crate_name in locked.keys() {
        let family = crate_name.split('-').next().unwrap_or(crate_name);
        if families.contains(family) && !listed.contains(crate_name.as_str()) {
            omitted.push(format!(
                "  {crate_name} {}: in the resolved tree and in a family the \
                 inventory enumerates, but no row probes it",
                locked[crate_name]
            ));
        }
    }
    problems.extend(omitted);

    assert!(
        problems.is_empty(),
        "the post-release audit's dependency inventory has drifted from the \
         resolved tree. Each row below means the crates.io probe checks a \
         version this build does not use, or checks nothing at all:\n{}",
        problems.join("\n")
    );
}

/// Extract a bash array literal `name=(\n  a\n  b\n)` from a workflow.
fn bash_array(workflow: &str, name: &str) -> Vec<String> {
    workflow
        .lines()
        .skip_while(|line| line.trim() != format!("{name}=("))
        .skip(1)
        .take_while(|line| line.trim() != ")")
        .map(|line| line.trim().to_owned())
        .filter(|entry| !entry.is_empty() && !entry.starts_with('#'))
        .collect()
}

/// bd-reality-core-convergence-1azkt.18: "no stale/unbound input can enter an
/// archive".
///
/// The release pipeline uses no cache, so there is no stale cache to rebuild
/// from. The reuse surface is instead artifacts handed from `build` to
/// `release`, and that job already defends it well: it counts the artifacts,
/// checks each target x suffix is present, runs `sha256sum --check`, and
/// cosign-verifies. Those guards are real.
///
/// What is NOT defended is the guards' OWN INPUTS. Three things must agree and
/// nothing holds them equal:
///
///   the build matrix `target:` entries
///   the `expected_targets` array in the release job
///   `expected_artifact_count`, a hardcoded literal rather than the product
///
/// They agree today. If they drift, the count check does fail closed -- but it
/// fails DURING A RELEASE, which is the most expensive place to discover a
/// typo. This moves that discovery to commit time.
#[test]
fn release_expected_assets_match_the_build_matrix() {
    let workflow = repo_file(".github/workflows/release.yml");

    let matrix_targets: std::collections::BTreeSet<String> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- target: "))
        .map(str::trim)
        .map(str::to_owned)
        .collect();
    let expected_targets: std::collections::BTreeSet<String> =
        bash_array(&workflow, "expected_targets")
            .into_iter()
            .collect();
    let suffixes = bash_array(&workflow, "expected_suffixes");

    // A zero-length parse means the workflow's shape moved and this test can no
    // longer read it. That is a different failure from a real drift and must
    // not be reported as agreement -- empty sets compare equal to each other.
    assert!(
        !matrix_targets.is_empty(),
        "parsed zero matrix targets from release.yml; the matrix format changed \
         and this test can no longer read it. Fix the parser before trusting it."
    );
    assert!(
        !expected_targets.is_empty() && !suffixes.is_empty(),
        "parsed zero expected_targets or expected_suffixes from release.yml; \
         the release job's format changed and this test can no longer read it."
    );

    let missing: Vec<&str> = matrix_targets
        .difference(&expected_targets)
        .map(String::as_str)
        .collect();
    let unexpected: Vec<&str> = expected_targets
        .difference(&matrix_targets)
        .map(String::as_str)
        .collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "release `expected_targets` has drifted from the build matrix.\n  \
         built but not expected (their artifacts would be unpublished): {missing:?}\n  \
         expected but not built (the release would fail waiting for them): {unexpected:?}"
    );

    // Not an invariant: whether release.yml declares expected_artifact_count is
    // a property of a file read at runtime, so this expect CAN fire. That is
    // deliberate -- its absence is exactly what this test exists to catch, the
    // panic message is the diagnostic, and the enclosing fn returns () so there
    // is nothing to propagate to. Cargo.toml:551 carves tests out of
    // expect_used for this case; src/lib.rs:12 does the same for the lib's own
    // cfg(test), which this separate tests/ crate does not inherit.
    #[allow(clippy::expect_used)]
    let declared: usize = workflow
        .lines()
        .find_map(|line| line.trim().strip_prefix("expected_artifact_count="))
        .and_then(|value| value.trim().parse().ok())
        .expect("release.yml must declare expected_artifact_count");
    let product = expected_targets.len() * suffixes.len();
    assert_eq!(
        declared,
        product,
        "expected_artifact_count is a hardcoded literal and no longer equals \
         targets x suffixes: declared {declared}, but {} targets x {} suffixes \
         = {product}. Update the literal, or derive it.",
        expected_targets.len(),
        suffixes.len()
    );
}
