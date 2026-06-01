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

fn assert_order(haystack: &str, first: &str, second: &str) {
    let first_index = haystack
        .find(first)
        .unwrap_or_else(|| panic!("expected to find first marker {first:?}"));
    let second_index = haystack
        .find(second)
        .unwrap_or_else(|| panic!("expected to find second marker {second:?}"));
    assert!(
        first_index < second_index,
        "expected {first:?} to appear before {second:?}"
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
        "--tlog-upload=true",
        "--bundle ee-${{ matrix.target }}.provenance.json.sigstore.json",
        "ee-${{ matrix.target }}.provenance.json",
        "--insecure-ignore-tlog=false",
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
        "EE_INSTALL_REQUIRE_KEYLESS",
        "REQUIRE_KEYLESS=\"${EE_INSTALL_REQUIRE_KEYLESS:-0}\"",
        "--insecure-ignore-tlog=false",
        "Sigstore verified via pinned-key fallback",
        "${artifact_url%.tar.xz}.provenance.json",
        "${provenance_url}.sigstore.json",
        "cosign verify-blob",
        "provenance predicateType is not SLSA v1",
        "provenance subject sha256 does not match downloaded artifact",
        "provenance is missing Cargo.lock blake3 dependency",
        "--require-provenance cannot be combined with --no-verify",
        "EE_INSTALL_REQUIRE_KEYLESS=1 cannot be combined with --no-verify",
    ] {
        assert_contains(&installer, needle);
    }
    assert_order(
        &installer,
        "# Path 1..N: keyless identity-bound certs",
        "# Fallback path: pinned long-lived key",
    );
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

#[test]
fn release_signing_policy_documents_key_ceremony_and_keyless_only_mode() {
    let doc = repo_file("docs/security/release-signing.md");
    let changelog = repo_file("CHANGELOG.md");
    let workflow = repo_file(".github/workflows/release.yml");

    for needle in [
        "EE_INSTALL_REQUIRE_KEYLESS=1",
        "--insecure-ignore-tlog=false",
        "--tlog-upload=true",
        "Key Generation",
        "Rotation",
        "Revocation",
        "mode `0600`",
    ] {
        assert_contains(&doc, needle);
    }
    assert_contains(&changelog, "docs/security/release-signing.md");
    assert_contains(&workflow, "Set EE_INSTALL_REQUIRE_KEYLESS=1");
    assert_order(
        &workflow,
        "### Keyless (workflow-built releases",
        "### Pinned key (manual-cut fallback releases)",
    );
}
