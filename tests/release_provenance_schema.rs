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

    for needle in [
        "| Path | Status | Provenance | Tracking |",
        "SLSA provenance planned; installer supports `--require-provenance`",
        "SLSA provenance JSON and its Sigstore bundle",
    ] {
        assert_contains(&readme, needle);
    }

    for needle in [
        "Signed release provenance ready",
        "ee-<target>.provenance.json",
        "Cargo.lock BLAKE3",
        "install.sh --require-provenance",
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
