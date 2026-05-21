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
