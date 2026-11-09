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

/// The ADR must still cite the inventory it was DECIDED on, and the live
/// inventory must still carry a pinned hash of its own.
///
/// THIS ARM USED TO ASSERT SOMETHING THE DESIGN MAKES IMPOSSIBLE, and it was
/// red for 25 days because of it. It required the CURRENT
/// `tests/randomness_inventory.json` to contain `decided_on` — a hash computed
/// on 2026-05-13 — so the first regeneration of the inventory broke it. The
/// inventory has four revisions and the hash moved every time:
///
/// ```text
/// 3c2b4e4e9  2026-05-13  51a88547...  the hash this ADR cites
/// 6063c49ec  2026-10-14  6eb4d4a7...  <-- the arm went red here
/// 3016ab5f8  2026-10-18  661b2f6c...
/// 348a9eb7f  2026-10-20  dac7548c...
/// ```
///
/// Nothing saw it: `verify.sh` cannot run on the maintainer's host, hosted
/// Actions is off repo-wide, and this file reaches a runner only through
/// `tests/suites/integration_a_d.rs`, a 773-test shard nobody dispatches to
/// check a string. bd-2cdum closed four days into that window having
/// "verified separately that my regeneration broke no hash citation (nothing
/// references the old hash)". It was referenced in SIX tracked files, and the
/// arm was already red when that was written.
///
/// BUT ONLY THIS ONE WAS RED, AND THE OTHER FIVE MUST NOT BE "FIXED". A census
/// on 2026-11-08 found `decided_on` cited here, in the ADR as prose, in
/// `src/runtime/README.md` as prose, in `src/runtime/determinism.rs` as the
/// `pub const RANDOMNESS_INVENTORY_ROWS_CONTENT_HASH`, in
/// `tests/determinism_capability_token_unit.rs` asserting that const, and in
/// `tests/snapshots/determinism_token_doctest_output.snap` snapshotting it.
/// Those five are a closed, self-consistent triangle that never reads the
/// inventory — the const's own doc comment calls it "the N4.1 inventory hash
/// that drove the first deterministic-token design", i.e. a dated record. This
/// arm was the only citation in the tree that compared the hash against the
/// LIVE artifact, which is why it was the only one that could rot.
///
/// DO NOT "FIX" THIS BY UPDATING THE ADR TO THE CURRENT HASH. bd-2cdum's close
/// settled what this artifact is: a timestamped snapshot cited by hash, not a
/// live index, so staleness between snapshots is the design. The ADR is
/// internally consistent with the snapshot it names — it reports 316 sources
/// and 261 `systemtime` rows, and `3c2b4e4e9` holds exactly 316 and 261.
/// Pointing it at `dac7548c...` would pair those numbers with a 1198-row
/// inventory and make a correct record wrong.
///
/// What this arm CANNOT check is whether `decided_on` was ever really this
/// file's hash, because `include_str!` sees one revision and the answer lives
/// in git history. `scripts/check-adr-evidence-citations.py` checks that,
/// which is also why it can run where this test cannot. It sweeps every
/// tracked file, so it covers all six sites above and prints the revision and
/// date each cited hash belongs to.
///
/// KEEP THE SECOND ASSERTION EVEN IF IT LOOKS REDUNDANT.
/// `tests/randomness_inventory_schema_unit.rs` already checks that field
/// properly, by parsing. This one exists so `INVENTORY` stays used: it is the
/// only other reference to that const, and an unused const is a `dead_code`
/// warning, which the lint gate's `-D warnings` promotes to an error. Deleting
/// it turns a tidy-up into a red build.
#[test]
fn adr_0030_cites_randomness_inventory_hash() -> TestResult {
    let decided_on = "blake3-ish:51a8854727a5768008ba8269596e8666cc9ffdd88e8ac3f13101ad36434a3bfc";
    ensure_contains(ADR_0030, decided_on, "ADR 0030 inventory citation")?;
    ensure_contains(
        INVENTORY,
        "\"rows_content_hash\": \"blake3-ish:",
        "N4.1 inventory pinned hash field",
    )
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
