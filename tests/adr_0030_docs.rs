//! Verification hooks for ADR 0030 (Typed Determinism Capability Token).

type TestResult = Result<(), String>;

const ADR_0030: &str = include_str!("../docs/adr/0030-typed-determinism-capability-token.md");
const ADR_INDEX: &str = include_str!("../docs/adr/README.md");
const INVENTORY: &str = include_str!("randomness_inventory.json");

fn ensure_contains(haystack: &str, needle: &str, context: &str) -> TestResult {
    if haystack.contains(needle) {
        Ok(())
    } else {
        Err(format!("{context}: expected to find `{needle}`"))
    }
}

#[test]
fn adr_0030_exists_and_is_indexed() -> TestResult {
    ensure_contains(
        ADR_0030,
        "# ADR 0030: Typed Determinism Capability Token",
        "ADR 0030 title",
    )?;
    ensure_contains(
        ADR_INDEX,
        "0030-typed-determinism-capability-token.md",
        "ADR index",
    )
}

#[test]
fn adr_0030_has_required_sections() -> TestResult {
    for heading in [
        "## Context",
        "## Decision",
        "## Consequences",
        "## Rejected Alternatives",
        "## Verification",
    ] {
        ensure_contains(ADR_0030, heading, "ADR 0030 required section")?;
    }
    Ok(())
}

#[test]
fn adr_0030_cites_randomness_inventory_hash() -> TestResult {
    let inventory_hash =
        "blake3-ish:51a8854727a5768008ba8269596e8666cc9ffdd88e8ac3f13101ad36434a3bfc";
    ensure_contains(INVENTORY, inventory_hash, "N4.1 inventory")?;
    ensure_contains(ADR_0030, inventory_hash, "ADR 0030 inventory citation")
}

#[test]
fn adr_0030_documents_required_design_choices() -> TestResult {
    for phrase in [
        "move-only",
        "not `Sync`",
        "`Send`",
        "`child(label)`",
        "DeterministicClock",
        "DeterministicRng",
        "DeterministicOrder",
        "UUIDv7",
        "N4.3",
    ] {
        ensure_contains(ADR_0030, phrase, "ADR 0030 design choice")?;
    }
    Ok(())
}
