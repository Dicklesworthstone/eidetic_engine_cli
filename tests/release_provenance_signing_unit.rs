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
        "expected provenance signing contract to contain {needle:?}"
    );
}

#[test]
fn release_workflow_signs_provenance_with_same_sigstore_trust_boundary() {
    let workflow = repo_file(".github/workflows/release.yml");

    for needle in [
        "permissions:",
        "id-token: write",
        "uses: sigstore/cosign-installer@v3",
        "cosign sign-blob --yes",
        "--bundle ee-${{ matrix.target }}.provenance.json.sigstore.json",
        "ee-${{ matrix.target }}.provenance.json",
        "--certificate-identity-regexp \"$CERT_IDENTITY_REGEXP\"",
        "--certificate-oidc-issuer \"$CERT_OIDC_ISSUER\"",
        "https://token.actions.githubusercontent.com",
    ] {
        assert_contains(&workflow, needle);
    }
}

#[test]
fn installer_requires_provenance_when_requested() {
    let installer = repo_file("install.sh");

    for needle in [
        "--require-provenance",
        "EE_REQUIRE_PROVENANCE=1",
        "verify_provenance_bundle()",
        "verify_provenance_bundle \"$TMP/$TAR\" \"$URL\"",
        "${artifact_url%.tar.xz}.provenance.json",
        "${provenance_url}.sigstore.json",
        "cosign verify-blob",
        "provenance predicateType is not SLSA v1",
        "provenance subject sha256 does not match downloaded artifact",
        "provenance is missing Cargo.lock blake3 dependency",
        "--require-provenance cannot be combined with --no-verify",
    ] {
        assert_contains(&installer, needle);
    }
}

#[test]
fn e2e_script_exercises_static_and_asset_dir_provenance_paths() {
    let e2e = repo_file("scripts/e2e_release_provenance.sh");

    for needle in [
        "--static",
        "--asset-dir",
        "ee-*.tar.xz",
        ".provenance.json",
        ".provenance.json.sigstore.json",
        "https://slsa.dev/provenance/v1",
        "Cargo.lock blake3 dependency",
        "cosign verify-blob",
    ] {
        assert_contains(&e2e, needle);
    }
}
