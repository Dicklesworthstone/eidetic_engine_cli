//! Verification hooks for ADR 0029 (Handoff Capsule HMAC Keying).

type TestResult = Result<(), String>;

const ADR_0029: &str = include_str!("../docs/adr/0029-handoff-capsule-hmac.md");
const ADR_INDEX: &str = include_str!("../docs/adr/README.md");

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context}: expected to find `{needle}`"))
    }
}

#[test]
fn adr_0029_exists_and_is_indexed() -> TestResult {
    ensure_contains(
        ADR_0029,
        "# ADR 0029: Handoff Capsule HMAC Keying",
        "ADR 0029 title",
    )?;
    ensure_contains(ADR_INDEX, "0029-handoff-capsule-hmac.md", "ADR index")
}

#[test]
fn adr_0029_has_required_sections() -> TestResult {
    for heading in [
        "## Context",
        "## Decision",
        "## Consequences",
        "## Rejected Alternatives",
        "## Verification",
    ] {
        ensure_contains(ADR_0029, heading, "ADR 0029 required section")?;
    }
    Ok(())
}

#[test]
fn adr_0029_documents_keying_modes_and_serialization() -> TestResult {
    for phrase in [
        "`workspace_secret`",
        "`workspace_secret_machine_bound`",
        "`~/.local/share/ee/keys/handoff_machine_salt`",
        "big-endian `u32` byte length",
        "workspace_identity.fingerprint",
        "workspace_identity.repository_fingerprint",
        "HMAC-SHA256",
    ] {
        ensure_contains(ADR_0029, phrase, "ADR 0029 keying contract")?;
    }
    Ok(())
}

#[test]
fn adr_0029_rejects_public_identity_only_hmac_keys() -> TestResult {
    for phrase in [
        "That identity tuple is public",
        "secret key material",
        "Derive the HMAC key only from `workspace_identity`",
        "can edit the capsule could recompute the HMAC",
    ] {
        ensure_contains(ADR_0029, phrase, "ADR 0029 public-keying rejection")?;
    }
    Ok(())
}

#[test]
fn adr_0029_documents_fail_closed_resume_and_recovery() -> TestResult {
    for phrase in [
        "`ee handoff resume` verifies HMAC before exposing prompt fragments",
        "`handoff_hmac_missing`",
        "`handoff_capsule_tampered`",
        "`handoff_capsule_machine_mismatch`",
        "`strict_mode_no_salt_file`",
        "`--insecure-skip-hmac`",
        "`handoff.insecure_load`",
    ] {
        ensure_contains(ADR_0029, phrase, "ADR 0029 verification behavior")?;
    }
    Ok(())
}
