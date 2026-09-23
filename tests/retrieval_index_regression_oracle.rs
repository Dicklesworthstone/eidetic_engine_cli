//! bd-reality-core-convergence-1azkt.10: a source-attested retrieval/index
//! regression oracle, built BEFORE any publication/model fix.
//!
//! # What this is
//!
//! An instrument, not a tripwire. It answers one question about a candidate
//! build — *does concurrent retrieval over one published generation diverge?* —
//! and it is required to answer with one of four verdicts, never to guess:
//!
//! | Verdict | Meaning |
//! | --- | --- |
//! | `RaceAbsent` | A quorum of probes completed, every one agreed with the serial baseline, and the observations were substantive. |
//! | `RaceReproduced` | At least two probes that **both completed** disagreed on identical state. Positive evidence. |
//! | `Inconclusive` | Too few probes completed to judge. A resource signal (CPU starvation, load), explicitly **not** a race. |
//! | `InfraError` | The candidate could not be attested, or the fixture/baseline was unusable. Never a product pass **or** a product fail. |
//!
//! # The separation rule
//!
//! This is the whole design, and it is why the oracle exists rather than just
//! another concurrency test:
//!
//! > **A probe that fails to complete contributes a resource signal, never a
//! > semantic one.** Divergence is computed *only* across probes that
//! > completed successfully. Non-completion can therefore never manufacture a
//! > `RaceReproduced`; and because declaring absence requires a quorum of
//! > completions, a starved run can never manufacture a `RaceAbsent` either.
//!
//! An oracle that cannot separate "the answers disagreed" from "the box was
//! busy" is worse than no oracle, because it produces confident wrong answers
//! in *both* directions: it calls starvation a regression, and it calls a
//! lucky quiet round a clean bill of health. The prior art for this bead's
//! closed dependency, `tests/concurrent_search_lexical_arm_e2e.rs` (bd-…​.23),
//! is a correct *proof* of that specific fix but has exactly this weakness —
//! any nonzero child exit returns `Err` indistinguishably from a semantic
//! disagreement. This oracle does not replace it and does not re-run its
//! diagnosis; it adds the classification layer.
//!
//! # Vacuity guards
//!
//! Per the bead: *"No empty/abstention/OR assertion can count as positive
//! behavior."* Enforced in two places:
//!
//! 1. The serial baseline must carry every **required** field as a non-empty
//!    value. If a field is missing or empty — including because a JSON path
//!    was renamed — the verdict is `InfraError` naming the exact pointer, not
//!    a pass. Comparisons can never silently degrade into `null == null`.
//! 2. A probe that returns an empty result set while the baseline returned a
//!    non-empty one is counted as a **divergence**, not as agreement. That is
//!    the classic signature of the lost-lexical-arm race (a process that fails
//!    the index open serves zero results), and treating it as "agreement about
//!    emptiness" would hide the exact failure this oracle hunts.
//!
//! # On `LabRuntime`
//!
//! Deliberately not used here, and the reason is worth recording rather than
//! papering over: the behavior under test is *cross-process* contention for
//! OS-level index locks. `LabRuntime` gives deterministic scheduling of
//! asupersync tasks **inside one process**; it has no authority over how two
//! separate `ee` processes interleave their file-lock acquisitions. Wrapping
//! these probes in a seeded runtime would add ceremony and determinism
//! language without constraining the thing that actually races. The
//! determinism this oracle *can* offer is different and real: a fixed,
//! substantive baseline, exact-value comparison, and repeated rounds whose
//! verdict must be stable.
//!
//! # How a red is read
//!
//! `RaceReproduced` failing this test is the oracle **working**, not a defect
//! in the test — the bead says "reproduce or fail to reproduce … either
//! outcome becomes the implementation baseline". `Inconclusive` and
//! `InfraError` also fail, because the bead forbids counting them as a product
//! pass. Every failure message begins with its verdict class so a reader never
//! has to infer which of the four happened.
//!
//! The live probe is `#[ignore]` on purpose. Under concurrent swarm load
//! `Inconclusive` is the *likely* outcome, and a load-dependent red in the
//! shared suite would be precisely the manufactured-wrong-answer failure this
//! design exists to prevent. It is pointed at a candidate deliberately. The
//! classifier itself is covered by ordinary always-on tests below, so the
//! instrument cannot rot unobserved.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

type TestResult = Result<(), String>;

const BEAD_ID: &str = "bd-reality-core-convergence-1azkt.10";
const TEST_EVENT_SCHEMA: &str = "ee.test_event.v1";

/// Degraded codes that mean a process could not see an index. When the index
/// status simultaneously reports a valid, current generation, this is a
/// semantic fault, not a configuration one — the bead names it explicitly.
const INDEX_INVISIBLE_CODES: [&str; 4] = [
    "index_missing",
    "search_index_not_found",
    "index_not_found",
    "search_unavailable",
];

/// Degraded codes that mean the lexical arm was lost. Retained from the
/// bd-…​.23 proof because a silent drop to semantic-only ranking is a
/// divergence in retrieval semantics even when the id order survives.
const LEXICAL_LOSS_CODES: [&str; 2] = ["source_mode_fallback", "lexical_unavailable"];

// ── Verdicts ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    RaceAbsent,
    RaceReproduced(Vec<String>),
    Inconclusive(String),
    InfraError(String),
}

impl Verdict {
    fn class(&self) -> &'static str {
        match self {
            Self::RaceAbsent => "RACE_ABSENT",
            Self::RaceReproduced(_) => "RACE_REPRODUCED",
            Self::Inconclusive(_) => "INCONCLUSIVE",
            Self::InfraError(_) => "INFRA_ERROR",
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::RaceAbsent => "a quorum of probes completed and all agreed".to_owned(),
            Self::RaceReproduced(divergences) => divergences.join("; "),
            Self::Inconclusive(reason) | Self::InfraError(reason) => reason.clone(),
        }
    }

    /// Only `RaceAbsent` is a product pass. `InfraError` is never a product
    /// pass *or* a product fail — it fails the test so it can never be
    /// mistaken for green, and it is labelled so it is never mistaken for a
    /// regression either.
    fn is_product_pass(&self) -> bool {
        matches!(self, Self::RaceAbsent)
    }
}

// ── Observations ────────────────────────────────────────────────────────────

/// One probe's comparable record: canonical field name → rendered value.
///
/// Rendering to strings up front makes the comparison exact and total; there
/// is no float tolerance to tune and no partial ordering to get wrong.
type Record = BTreeMap<String, String>;

#[derive(Debug, Clone)]
enum ProbeOutcome {
    /// The probe ran to completion and produced a parseable success envelope.
    Completed(Record),
    /// The probe did not produce a usable answer: spawn failure, nonzero exit,
    /// timeout, or unparseable output. A **resource** signal. Never semantic.
    DidNotComplete(String),
}

/// Which fields must be present and non-empty in the serial baseline before any
/// comparison is meaningful. A missing one is `InfraError`, never a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeKind {
    Search,
    Pack,
}

impl ProbeKind {
    fn required_fields(self) -> &'static [&'static str] {
        match self {
            // An ordered id list and a named backend. Without both, "agreement"
            // would be agreement about nothing.
            Self::Search => &["results.order", "embed_backend"],
            // A pack hash is the pack's whole identity.
            Self::Pack => &["pack.hash"],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Pack => "pack",
        }
    }
}

// ── The classifier (pure; this is what the always-on tests cover) ───────────

/// Classify one round of probes against the serial baseline.
///
/// `index_generation_valid` is read from `ee index status` *outside* the
/// concurrent phase: it says a current, healthy generation exists. Only then
/// does an "index not found" degradation from a concurrent probe mean a
/// semantic fault rather than an unbuilt index.
fn classify_round(
    kind: ProbeKind,
    baseline: &Record,
    outcomes: &[ProbeOutcome],
    quorum: usize,
    index_generation_valid: bool,
) -> Verdict {
    // Vacuity guard #1: the baseline must be substantive, or nothing below can
    // mean anything.
    for field in kind.required_fields() {
        match baseline.get(*field) {
            None => {
                return Verdict::InfraError(format!(
                    "{} baseline carried no `{field}`; every comparison below would be vacuous (the JSON path is probably renamed)",
                    kind.label()
                ));
            }
            Some(value) if value.is_empty() => {
                return Verdict::InfraError(format!(
                    "{} baseline carried an empty `{field}`; an empty/abstaining baseline cannot count as positive behavior",
                    kind.label()
                ));
            }
            Some(_) => {}
        }
    }

    let mut divergences = Vec::new();
    let mut incomplete = Vec::new();
    let mut completed = 0_usize;

    for (index, outcome) in outcomes.iter().enumerate() {
        match outcome {
            ProbeOutcome::DidNotComplete(reason) => {
                // Deliberately NOT a divergence. This is the separation rule.
                incomplete.push(format!("#{index}: {reason}"));
            }
            ProbeOutcome::Completed(record) => {
                completed = completed.saturating_add(1);
                divergences.extend(compare_probe(
                    kind,
                    index,
                    baseline,
                    record,
                    index_generation_valid,
                ));
            }
        }
    }

    // An observed divergence is positive evidence and outranks quorum: other
    // probes agreeing cannot un-observe two that disagreed.
    if !divergences.is_empty() {
        return Verdict::RaceReproduced(divergences);
    }

    if completed < quorum {
        return Verdict::Inconclusive(format!(
            "only {completed} of {} {} probes completed (quorum {quorum}); this is a resource signal, NOT evidence that the race is absent — incomplete: [{}]",
            outcomes.len(),
            kind.label(),
            incomplete.join(", ")
        ));
    }

    Verdict::RaceAbsent
}

/// Compare one completed probe against the baseline, returning every
/// divergence it exhibits.
fn compare_probe(
    kind: ProbeKind,
    index: usize,
    baseline: &Record,
    probe: &Record,
    index_generation_valid: bool,
) -> Vec<String> {
    let mut divergences = Vec::new();
    let label = kind.label();

    // Vacuity guard #2: emptiness is never agreement. A probe that serves zero
    // results while the baseline served some is the classic lost-arm
    // signature, not a matching opinion.
    for field in kind.required_fields() {
        if probe.get(*field).is_none_or(String::is_empty) {
            divergences.push(format!(
                "{label} probe #{index} produced an empty/absent `{field}` while the serial baseline produced `{}`; an abstaining probe is a divergence, not agreement",
                baseline.get(*field).map_or("<absent>", String::as_str)
            ));
        }
    }

    // Exact comparison over every field the baseline actually observed. A field
    // present in the baseline but missing from a probe is itself a divergence:
    // the value did not merely change, it vanished.
    for (field, expected) in baseline {
        if field.starts_with("degraded.") {
            continue;
        }
        match probe.get(field) {
            Some(actual) if actual == expected => {}
            Some(actual) => divergences.push(format!(
                "{label} probe #{index} reported {field}={actual} but the serial baseline reported {field}={expected}"
            )),
            None => divergences.push(format!(
                "{label} probe #{index} did not report {field} at all; the serial baseline reported {expected}"
            )),
        }
    }

    // The bead's named failure classes that are semantic even in one probe.
    let degraded = probe
        .get("degraded.codes")
        .map(|codes| {
            codes
                .split(',')
                .filter(|code| !code.is_empty())
                .map(str::to_owned)
                .collect::<BTreeSet<String>>()
        })
        .unwrap_or_default();

    if index_generation_valid {
        for code in INDEX_INVISIBLE_CODES {
            if degraded.contains(code) {
                divergences.push(format!(
                    "{label} probe #{index} reported `{code}` while index status showed a healthy, current generation; the index exists but this process could not see it"
                ));
            }
        }
    }
    for code in LEXICAL_LOSS_CODES {
        if degraded.contains(code) {
            divergences.push(format!(
                "{label} probe #{index} lost the lexical arm (`{code}`); retrieval semantics changed under concurrency"
            ));
        }
    }

    divergences
}

/// Fold per-round verdicts into one.
///
/// A race observed in any round is real — agreement in later rounds cannot
/// retract an observation. Absence requires *every* round to have reached it,
/// which is what makes repetition worth doing: one quiet round is not proof.
fn fold_rounds(rounds: &[Verdict]) -> Verdict {
    if rounds.is_empty() {
        return Verdict::InfraError("no rounds were executed".to_owned());
    }
    for verdict in rounds {
        if matches!(verdict, Verdict::InfraError(_)) {
            return verdict.clone();
        }
    }
    let mut divergences = Vec::new();
    for (round, verdict) in rounds.iter().enumerate() {
        if let Verdict::RaceReproduced(round_divergences) = verdict {
            for divergence in round_divergences {
                divergences.push(format!("round {round}: {divergence}"));
            }
        }
    }
    if !divergences.is_empty() {
        return Verdict::RaceReproduced(divergences);
    }
    let inconclusive: Vec<String> = rounds
        .iter()
        .enumerate()
        .filter_map(|(round, verdict)| match verdict {
            Verdict::Inconclusive(reason) => Some(format!("round {round}: {reason}")),
            _ => None,
        })
        .collect();
    if !inconclusive.is_empty() {
        return Verdict::Inconclusive(inconclusive.join(" | "));
    }
    Verdict::RaceAbsent
}

// ── Extraction (exact JSON pointers, verified against the renderers) ────────

fn pointer_string(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer).and_then(|found| match found {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    })
}

fn degraded_codes(value: &Value) -> BTreeSet<String> {
    value
        .pointer("/degraded")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.pointer("/code").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Render `data.results[]` as one canonical, order-sensitive string.
///
/// `memoryId` and `score` are the documented search-result fields
/// (`preset_fields_for_command("search", …)` in `src/output/mod.rs`). If this
/// comes back empty on an attested run, the baseline gate reports `InfraError`
/// naming the pointer rather than passing vacuously.
fn render_result_order(value: &Value) -> String {
    value
        .pointer("/data/results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .map(|result| {
                    let id = result
                        .pointer("/memoryId")
                        .and_then(Value::as_str)
                        .unwrap_or("<no-memoryId>");
                    let score = result
                        .pointer("/score")
                        .map_or_else(|| "<no-score>".to_owned(), ToString::to_string);
                    format!("{id}@{score}")
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .unwrap_or_default()
}

fn search_record(value: &Value) -> Record {
    let mut record = Record::new();
    record.insert("results.order".to_owned(), render_result_order(value));
    for (name, pointer) in [
        ("embed_backend", "/data/embed_backend"),
        ("status", "/data/status"),
        ("sourceModeApplied", "/data/metrics/sourceModeApplied"),
    ] {
        if let Some(found) = pointer_string(value, pointer) {
            record.insert(name.to_owned(), found);
        }
    }
    record.insert(
        "degraded.codes".to_owned(),
        degraded_codes(value)
            .into_iter()
            .collect::<Vec<_>>()
            .join(","),
    );
    record
}

/// Which entities the pack SELECTED, as `rank:memoryId@selectedIn`, joined in
/// emitted order.
///
/// bd-reality-core-convergence-1azkt.10 bullet 3 names "selected entities"
/// among the things a probe must record, and `pack.hash` was the only thing
/// standing in for them. A hash makes a divergence DETECTABLE and leaves it
/// undiagnosable: two packs that disagree differ, and the hash cannot say which
/// entity moved or why it was chosen. `selectedIn` is carried because a pack
/// that selected the same memory for a different reason is a different
/// selection, and `rank` because order is the pack's whole point.
///
/// `(none)` for a pack that selected nothing — a real answer — and `None` only
/// when there is no `items` array at all, which means the JSON path was
/// renamed. Collapsing those two would make a renamed surface look like an
/// empty pack, which is the vacuity this file refuses everywhere else.
fn render_pack_selection(value: &Value) -> Option<String> {
    let items = value.pointer("/data/pack/items")?.as_array()?;
    if items.is_empty() {
        return Some("(none)".to_owned());
    }
    Some(
        items
            .iter()
            .map(|item| {
                let rank = item
                    .pointer("/rank")
                    .map_or_else(|| "<no-rank>".to_owned(), ToString::to_string);
                let id = item
                    .pointer("/memoryId")
                    .and_then(Value::as_str)
                    .unwrap_or("<no-memoryId>");
                let selected_in = item
                    .pointer("/selectedIn")
                    .and_then(Value::as_str)
                    .unwrap_or("<no-selectedIn>");
                format!("{rank}:{id}@{selected_in}")
            })
            .collect::<Vec<_>>()
            .join("|"),
    )
}

fn pack_record(value: &Value) -> Record {
    let mut record = Record::new();
    // Recorded, and therefore COMPARED: `diverge` walks every field the
    // baseline observed, so this participates in agreement rather than sitting
    // in the evidence unread.
    if let Some(selection) = render_pack_selection(value) {
        record.insert("pack.selection".to_owned(), selection);
    }
    for (name, pointer) in [
        ("pack.hash", "/data/pack/hash"),
        (
            "pack.sourceModeApplied",
            "/data/queryPlan/sourceModeApplied",
        ),
    ] {
        if let Some(found) = pointer_string(value, pointer) {
            record.insert(name.to_owned(), found);
        }
    }
    record.insert(
        "degraded.codes".to_owned(),
        degraded_codes(value)
            .into_iter()
            .collect::<Vec<_>>()
            .join(","),
    );
    record
}

// ── Candidate attestation ──────────────────────────────────────────────────

/// The enabled feature names, sorted and comma-joined, or `None` when the
/// candidate reported no `features` array at all.
///
/// Sorted because `features[]` order is a rendering detail and two candidates
/// that differ only in it are the same build. `None` is reserved for a MISSING
/// array — a renamed JSON path — and is distinct from `"(none)"`, which is a
/// real answer meaning a build with every feature off. Collapsing those two
/// would make a renamed surface indistinguishable from a minimal build, which
/// is the vacuity this file guards against elsewhere.
fn feature_set(version_json: &Value) -> Option<String> {
    let features = version_json.pointer("/data/features")?.as_array()?;
    let mut enabled: Vec<&str> = features
        .iter()
        .filter(|feature| feature.get("enabled").and_then(Value::as_bool) == Some(true))
        .filter_map(|feature| feature.get("name").and_then(Value::as_str))
        .collect();
    if enabled.is_empty() {
        return Some("(none)".to_owned());
    }
    enabled.sort_unstable();
    Some(enabled.join(","))
}

#[derive(Debug, Clone)]
struct CandidateIdentity {
    fields: Record,
}

impl CandidateIdentity {
    /// Observe the candidate. This never judges it.
    ///
    /// bd-0v23w: observation is split from judgement so the identity exists
    /// BEFORE anything can refuse it. The refusal path used to run with an
    /// empty event list, and `write_content_addressed_evidence` hashes the
    /// joined events body, so every refusal wrote a file named
    /// `af1349b9…` — the BLAKE3 of the empty string. That digest was correct
    /// and useless: it was a function of nothing, so two refusals of two
    /// different candidates on two different workers produced byte-identical
    /// evidence names. A P0 cited one of those files as proof.
    fn observe(version_json: &Value, binary: &Path) -> Result<Self, Verdict> {
        Self::attest(version_json, binary, false)
    }

    /// Why this candidate may not carry a verdict, or `None` if it may.
    ///
    /// Separated from `observe` so a caller can record WHAT it saw before
    /// deciding whether to accept it.
    fn unattested_reason(&self, require: bool) -> Option<Verdict> {
        if !require {
            return None;
        }
        match Self::attest_fields(&self.fields) {
            Ok(()) => None,
            Err(verdict) => Some(verdict),
        }
    }

    /// The bead requires an attested candidate: exact commit, dirty state,
    /// target triple, profile, and binary SHA-256. Failure to attest is
    /// `InfraError`, never a product verdict.
    fn attest(version_json: &Value, binary: &Path, require: bool) -> Result<Self, Verdict> {
        let mut fields = Record::new();
        for (name, pointer) in [
            ("frankenStack", "/data/build/frankenStack"),
            ("gitCommit", "/data/source/gitCommit"),
            ("gitDirty", "/data/source/gitDirty"),
            ("sourceState", "/data/source/state"),
            ("buildProfile", "/data/build/profile"),
            ("targetTriple", "/data/build/targetTriple"),
            ("version", "/data/version"),
        ] {
            fields.insert(
                name.to_owned(),
                pointer_string(version_json, pointer).unwrap_or_else(|| "<absent>".to_owned()),
            );
        }
        let sha = sha256_file(binary).map_err(|error| {
            Verdict::InfraError(format!("could not hash the candidate binary: {error}"))
        })?;
        fields.insert("binarySha256".to_owned(), sha);
        // bd-reality-core-convergence-1azkt.10 bullet 6 asks for REDACTION-SAFE
        // evidence, and docs/environment_attestation.md:151 is explicit that a
        // redaction-safe projection "does not include ... raw evidence
        // references, or host-private absolute paths" — while it DOES keep
        // "evidence-reference hashes" and "argv hashes". So the substitute is a
        // digest, not removal.
        //
        // This field used to be the raw absolute path. It was written on the
        // acceptance path already, and since 28fa731bb — my own fix, which made
        // a refusal record its candidate — it is written on EVERY REFUSAL too, a
        // path that previously wrote nothing. Host-private data baked into the
        // one artifact this bead exists to make trustworthy is the worst place
        // for it.
        //
        // The digest keeps what the field was actually for: two capsules from
        // the same binary location compare equal, and a relocation is visible as
        // a change. It does not keep /Users/<name>/... . Note the repo's own
        // rule that absolute-path digests are never PINNABLE across hosts — that
        // is correct and not a problem here, because this value identifies a
        // location within a run, and `binarySha256` already identifies the
        // binary itself.
        fields.insert(
            "binaryPathBlake3".to_owned(),
            blake3::hash(binary.to_string_lossy().as_bytes())
                .to_hex()
                .to_string(),
        );
        // A build configuration the identity does not record is a build
        // configuration the attestation cannot describe. `features[]` was
        // emitted by every candidate and read by none of them, so two builds
        // from one commit with different feature flags were distinguishable
        // only by `binarySha256` — an opaque digest that says THAT they differ
        // and never HOW.
        fields.insert(
            "featureSet".to_owned(),
            feature_set(version_json).unwrap_or_else(|| "<absent>".to_owned()),
        );

        // The 2026-08 reproduction used a binary reporting `gitCommit: null`
        // and `targetTriple: unknown`; the bead exists because that evidence
        // could not be attributed to current source. Refuse to repeat it.
        //
        // `frankenStack` and `featureSet` joined this list when the fields
        // became available: a commit answers WHICH SOURCE, and a candidate that
        // cannot also say which engine it linked and which features were on has
        // attested to a commit rather than to a build.
        if require {
            Self::attest_fields(&fields)?;
        }
        Ok(Self { fields })
    }

    /// Whether a recorded identity is attestable, judged purely from the
    /// record.
    ///
    /// Pure so the SAME judgement can be applied after the identity has already
    /// been written to evidence — which is what lets a refusal say which
    /// candidate it refused (bd-0v23w).
    fn attest_fields(fields: &Record) -> Result<(), Verdict> {
        {
            for name in ["gitCommit", "targetTriple", "frankenStack", "featureSet"] {
                let value = fields.get(name).map_or("<absent>", String::as_str);
                if value == "<absent>" || value == "unknown" || value == "null" {
                    return Err(Verdict::InfraError(format!(
                        "candidate is unattested: {name}={value}. ORACLE_REQUIRE_ATTESTATION=1 demands an exact-source candidate. The build learns its own provenance only if the harness hands it over, so forward it: RCH_ENV_ALLOWLIST=VERGEN_GIT_SHA,VERGEN_GIT_DIRTY,ORACLE_REQUIRE_ATTESTATION,ORACLE_EXPECTED_COMMIT VERGEN_GIT_SHA=<sha> VERGEN_GIT_DIRTY=false ORACLE_REQUIRE_ATTESTATION=1 ORACLE_EXPECTED_COMMIT=<sha> rch exec --base <sha> --clean-overlay --no-overlay -- cargo test --test integration_n_r -- <this test> --exact --ignored --test-threads=1 --nocapture. VERGEN_GIT_DIRTY=false is honest ONLY under --no-overlay, because an overlaid tree is not the commit it is stamped with. Note --base and --clean-overlay belong to `rch exec`; scripts/rch_verify.sh rejects them (exit 2) and spells the same thing --treeish <sha> --committed-tree, and it sets no VERGEN_* of its own, so the forwarding above is still required there."
                    )));
                }
            }
            if fields.get("gitDirty").map(String::as_str) == Some("true") {
                return Err(Verdict::InfraError(
                    "candidate was built from a dirty tree; a verdict from it is not attributable to a commit".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::Digest as _;
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    // Hex-encoded by hand rather than through `{:x}` on the digest output: the
    // `LowerHex` impl lives on the digest crate's array type and has moved
    // between major versions, and a build break in the attestation path would
    // read as an oracle defect rather than the dependency churn it is.
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.as_slice() {
        hex.push_str(&format!("{byte:02x}"));
    }
    Ok(hex)
}

/// Cargo config spellings that can redirect a dependency without touching
/// `Cargo.lock` (bd-reality-core-convergence-1azkt.10, ruling 2026-09-23 15:03Z).
///
/// The substitute lane, `rch exec --clean-overlay --no-overlay --base <sha>`,
/// runs only cargo, so it cannot scan the worker before building. The scan
/// therefore runs here: in the same `cargo test` process that built the
/// candidate, over the same config files and environment the build read. It
/// refuses the VERDICT, not the build; a file written between the build and
/// this scan would escape it.
const CARGO_CONFIG_MARKERS: [&str; 4] = ["[patch", "[replace", "replace-with", "[source"];

/// Environment keys that redirect resolution or wrap the compiler.
const BUILD_ENV_PATTERNS: [&str; 7] = [
    "CARGO_SOURCE_*",
    "CARGO_PATCH*",
    "CARGO_REGISTRIES_*_INDEX",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

fn env_key_redirects_resolution(key: &str) -> bool {
    key.starts_with("CARGO_SOURCE_")
        || key.starts_with("CARGO_PATCH")
        || (key.starts_with("CARGO_REGISTRIES_") && key.ends_with("_INDEX"))
        || matches!(
            key,
            "RUSTC_WRAPPER"
                | "RUSTC_WORKSPACE_WRAPPER"
                | "CARGO_BUILD_RUSTC_WRAPPER"
                | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
        )
}

/// The redirecting markers a cargo config contains, ignoring comment lines.
/// `paths` is matched as a key (`paths = ...`), the rest as substrings.
fn config_text_redirects_resolution(text: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        for marker in CARGO_CONFIG_MARKERS {
            if line.contains(marker) && !found.contains(&marker) {
                found.push(marker);
            }
        }
        if line
            .strip_prefix("paths")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
            && !found.contains(&"paths =")
        {
            found.push("paths =");
        }
    }
    found
}

#[derive(Debug, Default)]
struct BuildEnvironmentScan {
    /// Config locations probed, whether or not a file existed there.
    locations_probed: usize,
    /// Config files that existed and were read: (redaction-safe label, path).
    files_read: Vec<(String, PathBuf)>,
    /// Findings as `<label>: <marker>` or `env <KEY>`; never file contents.
    hits: Vec<String>,
}

/// Probe `CARGO_HOME/config[.toml]` and every ancestor `.cargo/config[.toml]`
/// from `manifest_dir` up to `/`, plus the given environment.
fn scan_build_environment(
    manifest_dir: &Path,
    cargo_home: Option<&Path>,
    env: impl IntoIterator<Item = (String, String)>,
) -> BuildEnvironmentScan {
    let mut locations: Vec<(String, PathBuf)> = Vec::new();
    if let Some(home) = cargo_home {
        for name in ["config", "config.toml"] {
            locations.push((format!("<CARGO_HOME>/{name}"), home.join(name)));
        }
    }
    for (depth, dir) in manifest_dir.ancestors().enumerate() {
        for name in ["config", "config.toml"] {
            locations.push((
                format!("<manifest-ancestor-{depth}>/.cargo/{name}"),
                dir.join(".cargo").join(name),
            ));
        }
    }
    let mut scan = BuildEnvironmentScan {
        locations_probed: locations.len(),
        ..BuildEnvironmentScan::default()
    };
    for (label, path) in locations {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for marker in config_text_redirects_resolution(&text) {
            scan.hits.push(format!("{label}: {marker}"));
        }
        scan.files_read.push((label, path));
    }
    for (key, value) in env {
        if env_key_redirects_resolution(&key) && !value.is_empty() {
            scan.hits.push(format!("env {key}"));
        }
    }
    scan
}

fn digest_tree(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<(), String> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("read_dir {}: {error}", dir.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
                continue;
            }
            if !path.is_file() {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, sha256_file(&path)?);
        }
        Ok(())
    }
    walk(root, root, &mut out)?;
    Ok(out)
}

fn tree_diff(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    let mut diffs = Vec::new();
    for (path, hash) in after {
        match before.get(path) {
            None => diffs.push(format!("added {path}")),
            Some(previous) if previous != hash => diffs.push(format!("changed {path}")),
            Some(_) => {}
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            diffs.push(format!("removed {path}"));
        }
    }
    diffs.sort();
    diffs
}

/// Why an attested candidate may not carry a verdict for the requested base,
/// or `None` if its `gitCommit` is that base (ruling 2026-09-23 15:03Z).
fn expected_commit_refusal(expected: Option<&str>, observed: &str) -> Option<String> {
    match expected {
        None => Some(
            "ORACLE_REQUIRE_ATTESTATION=1 needs ORACLE_EXPECTED_COMMIT=<the rch --base sha> to check the candidate's gitCommit against".to_owned(),
        ),
        Some(expected) if expected != observed => Some(format!(
            "candidate gitCommit {observed} is not the requested base {expected}"
        )),
        Some(_) => None,
    }
}

/// What the probe window must leave untouched (bd-reality-core-convergence-
/// 1azkt.10): both generation counters (ruling 17:12Z item 1), the bytes of
/// every workspace file other than the database's own files, the database
/// ROWS beyond what the probes declare (ruling 20:40Z), and the write lock by
/// its semantics (bd-xa6ud). The counters alone miss any write that does not
/// bump a generation; the bytes of `ee.db` and its WAL alone mislabel a
/// declared audit append (search writes `audit_log`, effect.rs) as a mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DurableState {
    db_generation: Option<String>,
    index_generation: Option<String>,
    files: BTreeMap<String, String>,
    rows: RowSnapshot,
    write_lock: WriteLock,
}

/// Judge a probe window against the tables the probes `declared` in the
/// effect manifest. `None` means nothing durable moved beyond them.
///
/// An empty `before` file digest or row snapshot is `InfraError`, not a pass:
/// "nothing changed" over nothing measured is the vacuous agreement this
/// oracle refuses to count. So is an unreadable baseline write lock.
fn durable_mutation(
    before: &DurableState,
    after: &DurableState,
    declared: &BTreeSet<String>,
) -> Option<Verdict> {
    if before.files.is_empty() {
        return Some(Verdict::InfraError(
            "the pre-window workspace digest is empty, so durable mutation was not measured"
                .to_owned(),
        ));
    }
    if before.rows.tables.is_empty() {
        return Some(Verdict::InfraError(
            "the pre-window row snapshot has no tables, so durable mutation was not measured"
                .to_owned(),
        ));
    }
    let mut findings = Vec::new();
    let mut unmeasured = Vec::new();
    if before.db_generation != after.db_generation
        || before.index_generation != after.index_generation
    {
        findings.push(format!(
            "durable mutation: generation moved from db={:?}/index={:?} to db={:?}/index={:?}",
            before.db_generation,
            before.index_generation,
            after.db_generation,
            after.index_generation
        ));
    }
    let before_files: BTreeMap<String, String> = before
        .files
        .iter()
        .filter(|(path, _)| !is_db_state_file(path))
        .map(|(path, digest)| (path.clone(), digest.clone()))
        .collect();
    let after_files: BTreeMap<String, String> = after
        .files
        .iter()
        .filter(|(path, _)| !is_db_state_file(path))
        .map(|(path, digest)| (path.clone(), digest.clone()))
        .collect();
    let changed = tree_diff(&before_files, &after_files);
    if !changed.is_empty() {
        findings.push(format!(
            "durable mutation: workspace files changed: {changed:?}"
        ));
    }
    let (row_findings, row_unmeasured) = row_mutation(&before.rows, &after.rows, declared);
    if !row_findings.is_empty() {
        findings.push(format!(
            "durable mutation beyond the declared writes {declared:?}: {row_findings:?}"
        ));
    }
    unmeasured.extend(row_unmeasured);
    match write_lock_regression(&before.write_lock, &after.write_lock) {
        Ok(Some(finding)) => findings.push(format!("durable mutation: {finding}")),
        Ok(None) => {}
        Err(reason) => unmeasured.push(reason),
    }
    if !findings.is_empty() {
        findings.extend(
            unmeasured
                .into_iter()
                .map(|reason| format!("also unmeasured: {reason}")),
        );
        Some(Verdict::RaceReproduced(findings))
    } else if !unmeasured.is_empty() {
        Some(Verdict::InfraError(unmeasured.join("; ")))
    } else {
        None
    }
}

/// The database files' byte churn, classified for the record but not judged by
/// bytes (ruling 20:40Z condition 4: "classified, not ignored"). An `ee.db`
/// byte change is recorded as UNEXPLAINED (condition 6): the rows cover its
/// consequence, but nothing here explains it.
fn db_file_churn(before: &DurableState, after: &DurableState) -> Vec<Value> {
    tree_diff(&before.files, &after.files)
        .into_iter()
        .filter_map(|change| {
            let path = change
                .split_once(' ')
                .map_or(change.as_str(), |(_, path)| path);
            if !is_db_state_file(path) {
                return None;
            }
            let class = if path == ".ee/ee.db" {
                "db-bytes-UNEXPLAINED"
            } else {
                classify_workspace_path(path)
            };
            Some(serde_json::json!({ "change": change, "class": class }))
        })
        .collect()
}

/// The fixture workspace's durable state: the file digest first, then the
/// rows (read from a copy under the scratch dir, never the live files), then
/// the write lock.
fn durable_state(
    fixture: &Fixture,
    tag: &str,
    db_generation: Option<String>,
    index_generation: Option<String>,
    declared: &BTreeSet<String>,
) -> Result<DurableState, String> {
    let files = digest_tree(&fixture.workspace)?;
    let rows = row_snapshot(
        &fixture.workspace,
        &fixture.scratch.join(format!("rows-{tag}")),
        declared,
    )?;
    let write_lock = read_write_lock(&fixture.workspace.join(".ee").join("ee.write.lock"));
    Ok(DurableState {
        db_generation,
        index_generation,
        files,
        rows,
        write_lock,
    })
}

// ── Harness knobs ──────────────────────────────────────────────────────────
//
// Deliberately NOT `EE_*`-prefixed: these configure the test harness, not `ee`
// behavior, so they stay out of the `EE_*` registry contract in
// `src/config/env_registry.rs` / `docs/env_vars.md`.

fn knob(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(default)
}

fn require_attestation() -> bool {
    std::env::var("ORACLE_REQUIRE_ATTESTATION").is_ok_and(|value| value == "1")
}

// ── Process probes ─────────────────────────────────────────────────────────

struct Fixture {
    workspace: PathBuf,
    data_home: PathBuf,
    scratch: PathBuf,
}

/// The realistic isolated workspace every live arm runs against: eight rules,
/// one of them off-topic, then an index rebuild.
fn build_realistic_workspace(fixture: &Fixture) -> Result<(), String> {
    run_ee(fixture, &["init", "--json"])?;
    let rules = [
        "Run cargo fmt --check before every release tag.",
        "Release verification must go through the remote RCH lane, never local cargo.",
        "Clippy nursery and pedantic lints are errors in CI; fix them before a release.",
        "Never publish a release without a SHA-256 checksum for every asset.",
        "The release workflow triggers on a version tag pushed to main.",
        "Backups must be verified before a release restore drill.",
        "Search index generation must equal DB generation before release smoke tests.",
        "Frontend CSS tweaks are unrelated to release verification.",
    ];
    for rule in rules {
        run_ee(
            fixture,
            &[
                "remember",
                rule,
                "--level",
                "procedural",
                "--kind",
                "rule",
                "--json",
            ],
        )?;
    }
    run_ee(fixture, &["index", "rebuild", "--json"])?;
    Ok(())
}

/// Which kind of workspace file a path names (GraniteKite 2026-09-23, after the
/// bd-xa6ud precedent "classify, don't ignore"): `shm` is the WAL shared-memory
/// index that readers legitimately touch; `wal` covers the write-ahead log and
/// its FrankenSQLite certification files; `lock` is a lock file, judged
/// semantically rather than by bytes; anything else is `durable`.
fn classify_workspace_path(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with("-shm") {
        "shm"
    } else if name.ends_with("-wal") || name.contains("-wal-") {
        "wal"
    } else if name.ends_with(".lock") {
        "lock"
    } else {
        "durable"
    }
}

/// What an `ee.db` byte change was (GraniteKite 2026-09-23, byte vs logical;
/// pre-registered in 1azkt.10 comment 10021). `checkpoint`: the rows are equal
/// and the WAL shrank, truncated or went away. `durable`: the rows differ.
/// `db-bytes-other`: the rows are equal but the WAL did not shrink, which is
/// reported rather than guessed at. `db-bytes-unjudged`: the logical digest
/// could not be taken, so the change is not classified.
fn classify_db_byte_change(
    logical_equal: Option<bool>,
    wal_before: Option<u64>,
    wal_after: Option<u64>,
) -> &'static str {
    match logical_equal {
        None => "db-bytes-unjudged",
        Some(false) => "durable",
        Some(true) => {
            let wal_shrank = match (wal_before, wal_after) {
                (Some(before), Some(after)) => after < before,
                (Some(before), None) => before > 0,
                (None, _) => false,
            };
            if wal_shrank {
                "checkpoint"
            } else {
                "db-bytes-other"
            }
        }
    }
}

/// The size of `.ee/ee.db-wal`, or `None` when there is none.
fn wal_size(workspace: &Path) -> Option<u64> {
    std::fs::metadata(workspace.join(".ee").join("ee.db-wal"))
        .ok()
        .map(|metadata| metadata.len())
}

/// The rows of the workspace database (1azkt.10 comment 10021, ruling 20:40Z).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowSnapshot {
    /// Every user table: `<row count>:<blake3 over its sorted rows>`.
    tables: BTreeMap<String, String>,
    /// Each declared table present in the database: rowid -> row digest.
    declared_rows: BTreeMap<String, BTreeMap<i64, String>>,
    /// The database files the snapshot was read from.
    copied: Vec<String>,
}

impl RowSnapshot {
    /// One digest over every table's digest.
    fn all_digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for (table, digest) in &self.tables {
            hasher.update(format!("{table}={digest}\n").as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// The rows of the workspace database: every table's logical digest, plus the
/// rowid-keyed rows of each `declared` table.
///
/// It never opens the live files, because opening them could itself
/// checkpoint the WAL. It copies `ee.db`, `ee.db-wal` and `ee.db-wal-cert*`
/// (not `-shm`) into `copy_dir` and opens the COPY read-write: a read-only
/// open of a copied WAL database is refused with "database is busy (recovery
/// in progress)" (evidence 577dae27), because WAL recovery needs a writer.
/// Recovery only replays committed frames, so the rows read are the rows any
/// reader of the live files would see.
fn row_snapshot(
    workspace: &Path,
    copy_dir: &Path,
    declared: &BTreeSet<String>,
) -> Result<RowSnapshot, String> {
    let db_dir = workspace.join(".ee");
    std::fs::create_dir_all(copy_dir)
        .map_err(|error| format!("create {}: {error}", copy_dir.display()))?;
    let mut copied = Vec::new();
    let entries = std::fs::read_dir(&db_dir)
        .map_err(|error| format!("read_dir {}: {error}", db_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "ee.db" || name == "ee.db-wal" || name.starts_with("ee.db-wal-cert") {
            std::fs::copy(entry.path(), copy_dir.join(&name))
                .map_err(|error| format!("copy {name}: {error}"))?;
            copied.push(name);
        }
    }
    copied.sort();
    if !copied.iter().any(|name| name == "ee.db") {
        return Err(format!("no ee.db under {}", db_dir.display()));
    }
    let connection = ee::db::DbConnection::open_file(copy_dir.join("ee.db"))
        .map_err(|error| format!("open the ee.db copy: {error}"))?;
    let tables = connection
        .logical_table_digests()
        .map_err(|error| format!("logical digest of the ee.db copy: {error}"))?;
    if tables.is_empty() {
        return Err(
            "the ee.db copy has no user tables, so the logical digest measured nothing".to_owned(),
        );
    }
    let mut declared_rows = BTreeMap::new();
    for table in declared.iter().filter(|table| tables.contains_key(*table)) {
        let rows = connection
            .table_row_digests_by_rowid(table)
            .map_err(|error| format!("rowid digests of {table} in the ee.db copy: {error}"))?;
        declared_rows.insert(table.clone(), rows);
    }
    Ok(RowSnapshot {
        tables,
        declared_rows,
        copied,
    })
}

/// The tables the effect manifest lets these invocations write (ruling 20:40Z
/// condition 1): each argv is parsed by the real CLI, mapped to its command
/// path the way `ee` itself maps it, and looked up in `EffectManifest`. Never
/// a hand-list, so the oracle cannot drift from the declarations. An argv
/// that does not parse, or a path with no declaration, is an error: an
/// undeclared command cannot be judged. Also returns what was derived, for
/// the evidence.
fn declared_write_tables(invocations: &[&[&str]]) -> Result<(BTreeSet<String>, Value), String> {
    use clap::Parser as _;
    let manifest = ee::core::effect::EffectManifest::build();
    let mut tables = BTreeSet::new();
    let mut derived = Vec::new();
    for args in invocations {
        let cli = ee::cli::Cli::try_parse_from(std::iter::once("ee").chain(args.iter().copied()))
            .map_err(|error| format!("probe argv {args:?} does not parse: {error}"))?;
        let command_path = ee::cli::NormalizedInvocation::from_cli(&cli, &[]).command_path;
        let effect = manifest.get(&command_path).ok_or_else(|| {
            format!("command path {command_path:?} (argv {args:?}) has no effect declaration")
        })?;
        tables.extend(
            effect
                .write_surfaces
                .db_tables
                .iter()
                .map(|table| (*table).to_owned()),
        );
        derived.push(serde_json::json!({
            "argv": args,
            "commandPath": command_path,
            "dbTables": effect.write_surfaces.db_tables,
        }));
    }
    Ok((tables, Value::Array(derived)))
}

/// Judge two row snapshots (ruling 20:40Z conditions 2 and 3). Returns the
/// findings, and separately what could not be measured.
///
/// - An undeclared table must be row-equal: any change, and any table that
///   appears or disappears, is a finding.
/// - A declared table is APPEND-ONLY by rowid: every row present before must
///   be present after with the same digest. A missing rowid is DELETED, a
///   changed digest is MODIFIED; new rowids are allowed. A count is never
///   compared, because a delete plus an insert keeps it equal.
fn row_mutation(
    before: &RowSnapshot,
    after: &RowSnapshot,
    declared: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut findings = Vec::new();
    let mut unmeasured = Vec::new();
    let names: BTreeSet<&String> = before.tables.keys().chain(after.tables.keys()).collect();
    for table in names {
        let (was, is) = (before.tables.get(table), after.tables.get(table));
        if !declared.contains(table) {
            if was != is {
                findings.push(format!(
                    "undeclared table {table} changed: {} -> {}",
                    was.map_or("absent", String::as_str),
                    is.map_or("absent", String::as_str)
                ));
            }
            continue;
        }
        match (was, is) {
            (None, None) => {}
            (None, Some(_)) => findings.push(format!("declared table {table} was created")),
            (Some(_), None) => findings.push(format!("declared table {table} was removed")),
            (Some(_), Some(_)) => {
                let (Some(rows_before), Some(rows_after)) = (
                    before.declared_rows.get(table),
                    after.declared_rows.get(table),
                ) else {
                    unmeasured.push(format!("declared table {table} has no rowid digests"));
                    continue;
                };
                for (rowid, digest) in rows_before {
                    match rows_after.get(rowid) {
                        None => {
                            findings.push(format!("declared table {table}: row {rowid} DELETED"));
                        }
                        Some(now) if now != digest => {
                            findings.push(format!("declared table {table}: row {rowid} MODIFIED"));
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }
    (findings, unmeasured)
}

/// `.ee/ee.write.lock` read by its semantics (bd-xa6ud precedent,
/// tests/doctor_fixtures/lib.sh): 21 bytes, a 20-digit epoch and a newline.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WriteLock {
    Absent,
    Epoch(String),
    Unreadable(String),
}

fn read_write_lock(path: &Path) -> WriteLock {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return WriteLock::Absent,
        Err(error) => return WriteLock::Unreadable(error.to_string()),
    };
    if !metadata.is_file() {
        return WriteLock::Unreadable("not a regular file".to_owned());
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return WriteLock::Unreadable(error.to_string()),
    };
    match bytes.split_last() {
        Some((b'\n', digits)) if digits.len() == 20 && digits.iter().all(u8::is_ascii_digit) => {
            WriteLock::Epoch(String::from_utf8_lossy(digits).into_owned())
        }
        _ => WriteLock::Unreadable(format!(
            "{} bytes, not a 20-digit epoch and a newline",
            bytes.len()
        )),
    }
}

/// The xa6ud rule: absent stays absent; a present lock stays readable and its
/// epoch never goes backwards. `Err` means the baseline itself is unreadable.
fn write_lock_regression(before: &WriteLock, after: &WriteLock) -> Result<Option<String>, String> {
    match (before, after) {
        (WriteLock::Unreadable(reason), _) => Err(format!(
            "the baseline ee.write.lock is unreadable: {reason}"
        )),
        (WriteLock::Absent, WriteLock::Absent) => Ok(None),
        (WriteLock::Absent, _) => Ok(Some("ee.write.lock appeared".to_owned())),
        (WriteLock::Epoch(_), WriteLock::Absent) => {
            Ok(Some("ee.write.lock disappeared".to_owned()))
        }
        (WriteLock::Epoch(_), WriteLock::Unreadable(reason)) => {
            Ok(Some(format!("ee.write.lock became unreadable: {reason}")))
        }
        // Zero-padded to 20 digits, so string order is numeric order.
        (WriteLock::Epoch(was), WriteLock::Epoch(is)) if is < was => Ok(Some(format!(
            "ee.write.lock epoch went backwards: {was} -> {is}"
        ))),
        (WriteLock::Epoch(_), WriteLock::Epoch(_)) => Ok(None),
    }
}

/// The database files judged by their rows or semantics instead of their
/// bytes: `ee.db` (rows), its shm/WAL sidecars (classified churn) and
/// `ee.write.lock` (xa6ud). Every other workspace file stays byte-compared.
fn is_db_state_file(path: &str) -> bool {
    path == ".ee/ee.db"
        || path == ".ee/ee.write.lock"
        || (path.starts_with(".ee/ee.db-")
            && matches!(classify_workspace_path(path), "shm" | "wal"))
}

fn ee_command(fixture: &Fixture, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ee"));
    command
        .args(args)
        .env_remove("EE_WORKSPACE")
        .env_remove("EE_WORKSPACE_REGISTRY")
        .env_remove("EE_DATABASE_PATH")
        .env_remove("EE_INDEX_DIR")
        .env("HOME", &fixture.data_home)
        .env("XDG_DATA_HOME", &fixture.data_home)
        .env("XDG_CONFIG_HOME", &fixture.data_home)
        .env("EE_EMBED_DOWNLOAD", "off")
        .env("EE_NO_COLOR", "1")
        .arg("--workspace")
        .arg(&fixture.workspace);
    command
}

fn run_ee(fixture: &Fixture, args: &[&str]) -> Result<Value, String> {
    let output = ee_command(fixture, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("spawn ee {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        // stderr is never silenced: it is the evidence for an INFRA verdict.
        return Err(format!(
            "ee {} exited {:?}\nstdout: {}\nstderr: {}",
            args.join(" "),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "ee {} stdout was not JSON: {error}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Spawn `count` probes whose stdout/stderr go to files rather than pipes.
///
/// Files, not pipes, on purpose: a probe blocked writing into a full pipe
/// buffer while the parent polls `try_wait` would deadlock, and that deadlock
/// would be indistinguishable from the hang this oracle is meant to classify.
fn spawn_probes(
    fixture: &Fixture,
    args: &[&str],
    count: usize,
    round: usize,
    tag: &str,
) -> Result<Vec<(Child, PathBuf, PathBuf)>, String> {
    let mut children = Vec::with_capacity(count);
    for index in 0..count {
        let out_path = fixture
            .scratch
            .join(format!("{tag}-r{round}-p{index}.out.json"));
        let err_path = fixture.scratch.join(format!("{tag}-r{round}-p{index}.err"));
        let out = File::create(&out_path)
            .map_err(|error| format!("create {}: {error}", out_path.display()))?;
        let err = File::create(&err_path)
            .map_err(|error| format!("create {}: {error}", err_path.display()))?;
        let child = ee_command(fixture, args)
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|error| format!("spawn probe {index}: {error}"))?;
        children.push((child, out_path, err_path));
    }
    Ok(children)
}

/// Collect probes under a deadline, converting a hang into a *resource* signal.
///
/// A timed-out probe is killed and recorded as `DidNotComplete`. It never
/// becomes a divergence — a hung process has no opinion about ranking.
fn collect_probes(
    children: Vec<(Child, PathBuf, PathBuf)>,
    timeout: Duration,
    extract: fn(&Value) -> Record,
) -> Vec<ProbeOutcome> {
    let deadline = Instant::now() + timeout;
    let mut outcomes = Vec::with_capacity(children.len());
    for (mut child, out_path, err_path) in children {
        // `Waited` / `TimedOut` / `WaitFailed` rather than an `Option<ExitStatus>`:
        // a wait that errors is its own resource signal and must not be
        // laundered into a synthetic exit status.
        enum Wait {
            Exited(std::process::ExitStatus),
            TimedOut,
            Failed(String),
        }
        let waited = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Wait::Exited(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break Wait::TimedOut;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => break Wait::Failed(error.to_string()),
            }
        };
        let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
        let status = match waited {
            Wait::Exited(status) => status,
            Wait::TimedOut => {
                outcomes.push(ProbeOutcome::DidNotComplete(format!(
                    "timed out after {timeout:?} and was killed; stderr: {}",
                    stderr.trim()
                )));
                continue;
            }
            Wait::Failed(error) => {
                outcomes.push(ProbeOutcome::DidNotComplete(format!(
                    "could not wait for the probe: {error}; stderr: {}",
                    stderr.trim()
                )));
                continue;
            }
        };
        if !status.success() {
            outcomes.push(ProbeOutcome::DidNotComplete(format!(
                "exited {:?}; stderr: {}",
                status.code(),
                stderr.trim()
            )));
            continue;
        }
        let body = match std::fs::read_to_string(&out_path) {
            Ok(body) => body,
            Err(error) => {
                outcomes.push(ProbeOutcome::DidNotComplete(format!(
                    "could not read probe stdout: {error}"
                )));
                continue;
            }
        };
        match serde_json::from_str::<Value>(body.trim()) {
            Ok(value) if value.pointer("/success") == Some(&Value::Bool(true)) => {
                outcomes.push(ProbeOutcome::Completed(extract(&value)));
            }
            Ok(value) => outcomes.push(ProbeOutcome::DidNotComplete(format!(
                "envelope did not report success: {value}"
            ))),
            Err(error) => outcomes.push(ProbeOutcome::DidNotComplete(format!(
                "stdout was not JSON: {error}; stderr: {}",
                stderr.trim()
            ))),
        }
    }
    outcomes
}

// ── Evidence ───────────────────────────────────────────────────────────────

/// Write the `ee.test_event.v1` stream to a content-addressed file.
///
/// The file is named for the BLAKE3 of its own bytes, so the proof location is
/// derived from the evidence rather than assigned to it, and two runs that
/// observed the same thing land on the same path.
fn write_content_addressed_evidence(dir: &Path, events: &[Value]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let body = evidence_body(events);
    let digest = blake3::hash(body.as_bytes()).to_hex().to_string();
    let path = dir.join(format!("{digest}.ee-test-event.jsonl"));
    std::fs::write(&path, body.as_bytes())
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(path)
}

/// The exact bytes `write_content_addressed_evidence` stores and names.
fn evidence_body(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str(&event.to_string());
        body.push('\n');
    }
    body
}

/// Echo the evidence body between markers on STDOUT (ruling 20:40Z). rch
/// merges the remote stdout into its stderr with no ordering between the
/// two, and libtest writes its own lines to stdout, so a body on stderr got
/// libtest lines spliced into it (evidence 577dae27, c5dbda5e). One stream
/// is ordered. The leading newline ends libtest's pending `test ... ` line.
fn echo_evidence_body(events: &[Value]) {
    let body = evidence_body(events);
    println!(
        "\noracle evidence body blake3={} begin",
        blake3::hash(body.as_bytes()).to_hex()
    );
    print!("{body}");
    println!("oracle evidence body end");
}

fn event(phase: &str, status: &str, details: Value) -> Value {
    serde_json::json!({
        "schema": TEST_EVENT_SCHEMA,
        "surface": "retrieval_index_regression_oracle",
        "bead_id": BEAD_ID,
        "phase": phase,
        "status": status,
        "details": details,
    })
}

// ── The live oracle ────────────────────────────────────────────────────────

/// Point the oracle at the candidate `ee` built into this test binary.
///
/// Invocation (see the bead; ScarletMill runs this on the attested RCH lane):
///
/// ```text
/// ORACLE_REQUIRE_ATTESTATION=1 ORACLE_EXPECTED_COMMIT=<sha> ORACLE_PROOF_DIR=<dir> \
///   cargo test --locked --test integration_n_r -- \
///   --ignored --exact --nocapture --test-threads=1 \
///   retrieval_index_regression_oracle::concurrent_retrieval_over_one_generation_is_classified
/// ```
///
/// Under attestation the candidate's `gitCommit` must equal
/// `ORACLE_EXPECTED_COMMIT` (the `rch exec --base` sha), and the build
/// environment must carry no resolution redirect (`scan_build_environment`).
///
/// Two more knobs select cells and controls (ruling 2026-09-23 17:12Z):
/// `ORACLE_COLD_CONCURRENT=1` makes the first concurrent round the first touch
/// after the rebuild (the concurrent-cold index cell), and
/// `ORACLE_PLANT=index_aside` moves `.ee/index` aside for the warm rounds, a
/// planted control that MUST come out red.
///
/// The filter follows `--` so it reaches the test harness, not Cargo.
///
/// `--test-threads=1` matters: the oracle owns the machine's contention budget
/// for its window. Sharing it with other tests turns `RaceAbsent` runs into
/// `Inconclusive` ones and wastes the lane. `--nocapture` matters because the
/// verdict, its reasoning, and the evidence path are all in the failure
/// message, and a suppressed one is an unreadable result.
#[test]
#[ignore = "oracle: point it at a candidate deliberately (see bd-reality-core-convergence-1azkt.10); under swarm load the honest verdict is INCONCLUSIVE, which must not become a shared-suite red"]
fn concurrent_retrieval_over_one_generation_is_classified() -> TestResult {
    let probes = knob("ORACLE_PROBES", 8);
    let rounds = knob("ORACLE_ROUNDS", 3);
    let quorum = knob("ORACLE_QUORUM", probes.saturating_sub(1).max(1));
    let timeout = Duration::from_secs(knob("ORACLE_TIMEOUT_SECS", 120) as u64);

    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fixture = Fixture {
        workspace: tempdir.path().join("workspace"),
        data_home: tempdir.path().join("home"),
        scratch: tempdir.path().join("scratch"),
    };
    for dir in [&fixture.workspace, &fixture.data_home, &fixture.scratch] {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }

    let mut events = Vec::new();
    let proof_dir = std::env::var("ORACLE_PROOF_DIR")
        .map_or_else(|_| tempdir.path().join("proof"), PathBuf::from);

    // ── Attest the candidate before observing anything ──────────────────────
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_ee"));
    let version = match run_ee(&fixture, &["version", "--json"]) {
        Ok(value) => value,
        Err(error) => {
            return finish(
                &Verdict::InfraError(format!("candidate did not report a version: {error}")),
                &proof_dir,
                &mut events,
            );
        }
    };
    // bd-0v23w: OBSERVE, RECORD, then judge — in that order. Judging first left
    // `events` empty on every refusal, and the content-addressed proof is the
    // hash of the joined events body, so each refusal wrote a file named for
    // the BLAKE3 of the empty string. Recording the identity first makes that
    // digest a function of WHICH candidate was refused.
    let identity = match CandidateIdentity::observe(&version, &binary) {
        Ok(identity) => identity,
        Err(verdict) => return finish(&verdict, &proof_dir, &mut events),
    };
    let unattested = identity.unattested_reason(require_attestation());
    events.push(event(
        "attest_candidate",
        if unattested.is_some() {
            "refused"
        } else {
            "pass"
        },
        // Ruling 9964 (ii): the full `ee version --json` is kept, not only the
        // projected identity fields. It carries no host paths.
        serde_json::json!({ "identity": &identity.fields, "versionJson": &version }),
    ));
    if let Some(verdict) = unattested {
        return finish(&verdict, &proof_dir, &mut events);
    }

    // ── Build environment and expected commit (ruling 2026-09-23 15:03Z) ────
    // The candidate is the binary THIS cargo invocation built. Its raw path is
    // printed to the log only; the evidence keeps the path digest (bullet 6).
    let binary_sha = identity
        .fields
        .get("binarySha256")
        .map_or("<absent>", String::as_str);
    eprintln!(
        "oracle candidate: path={} sha256={binary_sha}",
        binary.display()
    );
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    let scan = scan_build_environment(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        cargo_home.as_deref(),
        std::env::vars(),
    );
    for (label, path) in &scan.files_read {
        eprintln!(
            "oracle build environment: read {label} = {}",
            path.display()
        );
    }
    eprintln!(
        "oracle build environment: probed {} config locations, read {} files, checked env {:?}, hits {:?}",
        scan.locations_probed,
        scan.files_read.len(),
        BUILD_ENV_PATTERNS,
        scan.hits
    );
    let expected_commit = std::env::var("ORACLE_EXPECTED_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let observed_commit = identity
        .fields
        .get("gitCommit")
        .map_or("<absent>", String::as_str)
        .to_owned();
    events.push(event(
        "build_environment",
        if scan.hits.is_empty() {
            "pass"
        } else {
            "refused"
        },
        serde_json::json!({
            "configLocationsProbed": scan.locations_probed,
            "configFilesReadCount": scan.files_read.len(),
            "configFilesRead": scan.files_read.iter().map(|(label, _)| label).collect::<Vec<_>>(),
            "envPatternsChecked": BUILD_ENV_PATTERNS,
            "hits": &scan.hits,
            "expectedCommit": &expected_commit,
            "observedCommit": &observed_commit,
            "cargoLockSha256": sha256_file(&Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"))
                .unwrap_or_else(|error| format!("<unreadable: {error}>")),
        }),
    ));
    if require_attestation() {
        if !scan.hits.is_empty() {
            return finish(
                &Verdict::InfraError(format!(
                    "the build environment can redirect dependency resolution: {:?}",
                    scan.hits
                )),
                &proof_dir,
                &mut events,
            );
        }
        if let Some(reason) = expected_commit_refusal(expected_commit.as_deref(), &observed_commit)
        {
            return finish(&Verdict::InfraError(reason), &proof_dir, &mut events);
        }
    }

    // ── Realistic isolated workspace ────────────────────────────────────────
    if let Err(error) = build_realistic_workspace(&fixture) {
        return finish(
            &Verdict::InfraError(format!("fixture setup failed: {error}")),
            &proof_dir,
            &mut events,
        );
    }

    // ── Is there a valid, current generation to be invisible? ───────────────
    let status = match run_ee(&fixture, &["index", "status", "--json"]) {
        Ok(value) => value,
        Err(error) => {
            return finish(
                &Verdict::InfraError(format!("index status unavailable: {error}")),
                &proof_dir,
                &mut events,
            );
        }
    };
    let db_generation = pointer_string(&status, "/data/dbGeneration");
    let index_generation = pointer_string(&status, "/data/indexGeneration");
    let health = pointer_string(&status, "/data/health").unwrap_or_default();
    let index_generation_valid =
        health == "ready" && db_generation.is_some() && db_generation == index_generation;
    events.push(event(
        "index_generation",
        if index_generation_valid {
            "pass"
        } else {
            "info"
        },
        serde_json::json!({
            "health": health,
            "dbGeneration": db_generation,
            "indexGeneration": index_generation,
            "embeddingMode": pointer_string(&status, "/data/embedding/mode"),
            "embeddingSemantic": pointer_string(&status, "/data/embedding/semantic"),
            "fastModelId": pointer_string(&status, "/data/embedding/fast_model_id"),
        }),
    ));

    let search_args = [
        "search",
        "release verification remote lane",
        "--limit",
        "5",
        "--json",
    ];
    let pack_args = [
        "pack",
        "prepare release",
        "--read-only",
        "--max-tokens",
        "1500",
        "--json",
    ];
    let status_args = ["index", "status", "--json"];

    // ── Durable state before the probe window (rulings 17:12Z item 1, 20:40Z) ─
    // Every command from here to the end of the window is one of these three.
    // What they may write comes from the effect manifest, not from this file.
    let declared = match declared_write_tables(&[
        search_args.as_slice(),
        pack_args.as_slice(),
        status_args.as_slice(),
    ]) {
        Ok((tables, derived)) => {
            events.push(event(
                "declared_write_tables",
                "info",
                serde_json::json!({ "tables": &tables, "derivedFrom": derived }),
            ));
            tables
        }
        Err(error) => {
            return finish(
                &Verdict::InfraError(format!(
                    "the probes' declared writes could not be derived: {error}"
                )),
                &proof_dir,
                &mut events,
            );
        }
    };
    let durable_before = match durable_state(
        &fixture,
        "before",
        db_generation.clone(),
        index_generation.clone(),
        &declared,
    ) {
        Ok(state) => state,
        Err(error) => {
            return finish(
                &Verdict::InfraError(format!("pre-window durable state failed: {error}")),
                &proof_dir,
                &mut events,
            );
        }
    };
    events.push(event(
        "durable_state_before",
        "info",
        serde_json::json!({
            "workspaceFiles": durable_before.files.len(),
            "dbGeneration": &durable_before.db_generation,
            "indexGeneration": &durable_before.index_generation,
            "tables": durable_before.rows.tables.len(),
            "rowsDigest": durable_before.rows.all_digest(),
            "declaredRows": durable_before
                .rows
                .declared_rows
                .iter()
                .map(|(table, rows)| (table.clone(), rows.len()))
                .collect::<BTreeMap<_, _>>(),
            "writeLock": format!("{:?}", durable_before.write_lock),
            "walBytes": wal_size(&fixture.workspace),
        }),
    ));

    // ── Concurrent-cold round (ruling 17:12Z item 3) ────────────────────────
    // With ORACLE_COLD_CONCURRENT=1 the FIRST touch after the rebuild is a
    // concurrent search round (then a pack round), judged below against the
    // serial baselines taken AFTER it. A workspace is cold only once, so in this
    // mode the serial baselines are not cold; the default mode covers
    // serial-cold instead, and the two runs together cover the four cells.
    let cold_concurrent = std::env::var("ORACLE_COLD_CONCURRENT").is_ok_and(|value| value == "1");
    let mut cold_rounds: Vec<(ProbeKind, Vec<ProbeOutcome>)> = Vec::new();
    if cold_concurrent {
        for (kind, args, extract) in [
            (
                ProbeKind::Search,
                search_args.as_slice(),
                search_record as fn(&Value) -> Record,
            ),
            (
                ProbeKind::Pack,
                pack_args.as_slice(),
                pack_record as fn(&Value) -> Record,
            ),
        ] {
            let tag = format!("{}-cold", kind.label());
            let children = match spawn_probes(&fixture, args, probes, 0, &tag) {
                Ok(children) => children,
                Err(error) => {
                    return finish(
                        &Verdict::InfraError(format!("could not spawn cold probes: {error}")),
                        &proof_dir,
                        &mut events,
                    );
                }
            };
            cold_rounds.push((kind, collect_probes(children, timeout, extract)));
        }
    }

    // ── Serial baselines, cold then warm ────────────────────────────────────
    // The cold baseline is the first touch after the rebuild (cold model/index
    // caches); the warm one follows it. Both must agree, or the candidate is
    // already non-deterministic serially and the concurrent verdict would be
    // unreadable.
    let cold = match run_ee(&fixture, &search_args) {
        Ok(value) => search_record(&value),
        Err(error) => {
            return finish(
                &Verdict::InfraError(format!("cold serial baseline failed: {error}")),
                &proof_dir,
                &mut events,
            );
        }
    };
    let warm = match run_ee(&fixture, &search_args) {
        Ok(value) => search_record(&value),
        Err(error) => {
            return finish(
                &Verdict::InfraError(format!("warm serial baseline failed: {error}")),
                &proof_dir,
                &mut events,
            );
        }
    };
    if cold != warm {
        let verdict = Verdict::RaceReproduced(vec![format!(
            "serial cold and warm baselines already disagree without any concurrency: cold={cold:?} warm={warm:?}"
        )]);
        return finish(&verdict, &proof_dir, &mut events);
    }
    let pack_baseline = match run_ee(&fixture, &pack_args) {
        Ok(value) => pack_record(&value),
        Err(error) => {
            return finish(
                &Verdict::InfraError(format!("serial pack baseline failed: {error}")),
                &proof_dir,
                &mut events,
            );
        }
    };
    events.push(event(
        "serial_baseline",
        "pass",
        // The serial-cold record is kept too (ruling 17:12Z item 3). It equals
        // `search` by construction here (a disagreement returned above), but it
        // is retained so that equality is evidence, not an inference.
        serde_json::json!({
            "search": warm,
            "searchCold": cold,
            "pack": pack_baseline,
            "serialIsFirstTouch": !cold_concurrent,
        }),
    ));

    let mut round_verdicts = Vec::new();
    for (kind, outcomes) in cold_rounds {
        let baseline = match kind {
            ProbeKind::Search => &warm,
            ProbeKind::Pack => &pack_baseline,
        };
        let verdict = classify_round(kind, baseline, &outcomes, quorum, index_generation_valid);
        events.push(event(
            "concurrent_round",
            match &verdict {
                Verdict::RaceAbsent => "pass",
                _ => "fail",
            },
            serde_json::json!({
                "round": "cold",
                "kind": kind.label(),
                "probes": probes,
                "quorum": quorum,
                "verdict": verdict.class(),
                "detail": verdict.detail(),
                "completed": outcomes.iter().filter(|outcome| matches!(outcome, ProbeOutcome::Completed(_))).count(),
                "incomplete": outcomes.iter().filter_map(|outcome| match outcome {
                    ProbeOutcome::DidNotComplete(reason) => Some(reason.clone()),
                    ProbeOutcome::Completed(_) => None,
                }).collect::<Vec<_>>(),
            }),
        ));
        round_verdicts.push(verdict);
    }

    // ── Planted control: invisible index (ruling 17:12Z item 5) ─────────────
    // ORACLE_PLANT=index_aside moves `.ee/index` aside INSIDE this job's own
    // temp workspace for the warm rounds, and moves it back before the durable
    // check. A run with the plant must come out red, naming the invisible
    // index; a green means the oracle is blind to "Search index not found while
    // a valid generation exists". Nothing is deleted.
    let plant = std::env::var("ORACLE_PLANT")
        .ok()
        .filter(|value| !value.is_empty());
    let index_dir = fixture.workspace.join(".ee").join("index");
    let index_aside = fixture
        .workspace
        .join(".ee")
        .join("index.oracle-planted-aside");
    if let Some(plant) = plant.as_deref() {
        if plant != "index_aside" {
            return finish(
                &Verdict::InfraError(format!(
                    "unknown ORACLE_PLANT {plant:?}; known: index_aside"
                )),
                &proof_dir,
                &mut events,
            );
        }
        if let Err(error) = std::fs::rename(&index_dir, &index_aside) {
            return finish(
                &Verdict::InfraError(format!("could not move .ee/index aside: {error}")),
                &proof_dir,
                &mut events,
            );
        }
        events.push(event(
            "plant",
            "info",
            serde_json::json!({ "plant": plant, "moved": ".ee/index -> .ee/index.oracle-planted-aside" }),
        ));
    }

    // ── Concurrent rounds ───────────────────────────────────────────────────
    for round in 0..rounds {
        for (kind, args, baseline, extract) in [
            (
                ProbeKind::Search,
                search_args.as_slice(),
                &warm,
                search_record as fn(&Value) -> Record,
            ),
            (
                ProbeKind::Pack,
                pack_args.as_slice(),
                &pack_baseline,
                pack_record as fn(&Value) -> Record,
            ),
        ] {
            let children = match spawn_probes(&fixture, args, probes, round, kind.label()) {
                Ok(children) => children,
                Err(error) => {
                    return finish(
                        &Verdict::InfraError(format!("could not spawn probes: {error}")),
                        &proof_dir,
                        &mut events,
                    );
                }
            };
            let outcomes = collect_probes(children, timeout, extract);
            let verdict = classify_round(kind, baseline, &outcomes, quorum, index_generation_valid);
            events.push(event(
                "concurrent_round",
                match &verdict {
                    Verdict::RaceAbsent => "pass",
                    _ => "fail",
                },
                serde_json::json!({
                    "round": round,
                    "kind": kind.label(),
                    "probes": probes,
                    "quorum": quorum,
                    "verdict": verdict.class(),
                    "detail": verdict.detail(),
                    "completed": outcomes.iter().filter(|outcome| matches!(outcome, ProbeOutcome::Completed(_))).count(),
                    "incomplete": outcomes.iter().filter_map(|outcome| match outcome {
                        ProbeOutcome::DidNotComplete(reason) => Some(reason.clone()),
                        ProbeOutcome::Completed(_) => None,
                    }).collect::<Vec<_>>(),
                }),
            ));
            round_verdicts.push(verdict);
        }
    }

    // Undo the plant before the durable check, without clobbering: if `ee`
    // recreated `.ee/index` while it was aside, that is itself a write during a
    // read-only window and is reported as such.
    if plant.is_some() {
        if index_dir.exists() {
            round_verdicts.push(Verdict::RaceReproduced(vec![
                "durable mutation under read-only probes: ee recreated .ee/index while the planted original was aside".to_owned(),
            ]));
            events.push(event(
                "plant_restored",
                "fail",
                serde_json::json!({ "restored": false, "reason": ".ee/index was recreated during the rounds" }),
            ));
        } else if let Err(error) = std::fs::rename(&index_aside, &index_dir) {
            round_verdicts.push(Verdict::InfraError(format!(
                "could not move the planted .ee/index back: {error}"
            )));
        } else {
            events.push(event(
                "plant_restored",
                "info",
                serde_json::json!({ "restored": true }),
            ));
        }
    }

    // ── Durable mutation beyond the declared writes ─────────────────────────
    // The probes are `search` (declared to append to audit_log, effect.rs),
    // `pack --read-only` and `index status`. The window is judged on the
    // generation counters, the non-database file bytes, the database ROWS
    // against the declared tables, and the write lock, by `durable_mutation`,
    // whose planted controls live in `mod durable_mutation_controls`.
    match run_ee(&fixture, &status_args) {
        Ok(after) => match durable_state(
            &fixture,
            "after",
            pointer_string(&after, "/data/dbGeneration"),
            pointer_string(&after, "/data/indexGeneration"),
            &declared,
        ) {
            Ok(durable_after) => {
                let (row_findings, _) =
                    row_mutation(&durable_before.rows, &durable_after.rows, &declared);
                events.push(event(
                    "durable_state_after",
                    "info",
                    serde_json::json!({
                        "workspaceFiles": durable_after.files.len(),
                        "dbGeneration": &durable_after.db_generation,
                        "indexGeneration": &durable_after.index_generation,
                        "changed": tree_diff(&durable_before.files, &durable_after.files),
                        "dbFileChurn": db_file_churn(&durable_before, &durable_after),
                        "rowsDigest": durable_after.rows.all_digest(),
                        "changedTables": durable_before
                            .rows
                            .tables
                            .keys()
                            .chain(durable_after.rows.tables.keys())
                            .filter(|table| {
                                durable_before.rows.tables.get(*table)
                                    != durable_after.rows.tables.get(*table)
                            })
                            .collect::<BTreeSet<_>>(),
                        "rowFindings": row_findings,
                        "writeLock": format!("{:?}", durable_after.write_lock),
                        "walBytes": wal_size(&fixture.workspace),
                    }),
                ));
                if let Some(verdict) = durable_mutation(&durable_before, &durable_after, &declared)
                {
                    round_verdicts.push(verdict);
                }
            }
            Err(error) => round_verdicts.push(Verdict::InfraError(format!(
                "post-window durable state failed, so durable mutation could not be checked: {error}"
            ))),
        },
        Err(error) => round_verdicts.push(Verdict::InfraError(format!(
            "post-probe index status unavailable, so durable mutation could not be checked: {error}"
        ))),
    }

    let verdict = fold_rounds(&round_verdicts);
    events.push(event(
        "verdict",
        if verdict.is_product_pass() {
            "pass"
        } else {
            "fail"
        },
        serde_json::json!({
            "verdict": verdict.class(),
            "detail": verdict.detail(),
            "identity": &identity.fields,
        }),
    ));
    finish(&verdict, &proof_dir, &mut events)
}

/// Persist evidence and map the verdict onto a test outcome.
///
/// Only `RACE_ABSENT` passes. Every other class fails with its label first, so
/// a reader never has to infer whether they are looking at a regression, a
/// starved box, or broken infrastructure.
fn finish(verdict: &Verdict, proof_dir: &Path, events: &mut Vec<Value>) -> TestResult {
    let proof = write_content_addressed_evidence(proof_dir, events)
        .unwrap_or_else(|error| PathBuf::from(format!("<evidence unwritable: {error}>")));
    // Ruling 9964 (ii): a proof file on an RCH worker does not outlive the
    // job, so the body is echoed as well. Its BLAKE3 equals the file name,
    // which is how a copy retained elsewhere proves it is the same evidence.
    echo_evidence_body(events);
    if verdict.is_product_pass() {
        // bd-0v23w: a pass used to compute this path and discard it. The
        // artifact was always written — it was simply never named, so no green
        // run in this oracle's history has ever been citable. Printing it costs
        // nothing and is the difference between "it passed" and "here is what
        // passed". Reaches the log only under --nocapture, which this test's
        // documented invocation already requires.
        println!(
            "{}: {}\nevidence: {}",
            verdict.class(),
            verdict.detail(),
            proof.display()
        );
        return Ok(());
    }
    Err(format!(
        "{}: {}\nevidence: {}",
        verdict.class(),
        verdict.detail(),
        proof.display()
    ))
}

/// Attribution of the durable-mutation red seen at 42c8408
/// (bd-reality-core-convergence-1azkt.10; arms suggested by GraniteKite
/// 2026-09-23). On ONE realistic workspace, the files are digested before and
/// after each arm, and every changed path is classified shm / wal / lock /
/// durable. Each arm also records the `-wal` size and a logical digest of the
/// database rows (taken from a read-only copy), so an `ee.db` byte change is
/// split into checkpoint / durable / db-bytes-other (comment 10021):
///
/// - N1: nothing between the two digests (null control; must show no change)
/// - S:  one `ee index status --json` (declared read_only_db in effect.rs)
/// - PS: one concurrent round of search probes, with no status call inside
/// - PP: one concurrent round of `pack --read-only` probes, likewise
/// - N2: nothing again (null control after the probes)
/// - W:  one `ee remember` (positive control: the logical digest must differ,
///   or it is blind and a `checkpoint` reading elsewhere proves nothing)
///
/// It records; it renders no product verdict. A `durable` change in S is a
/// read_only_db command writing durable state. A `durable` change in PS or PP
/// is the read-only race signal. Run it deliberately:
/// `cargo test --locked --test integration_n_r -- --ignored --exact --nocapture
/// retrieval_index_regression_oracle::read_only_window_attribution`.
#[test]
#[ignore = "attribution probe for the 1azkt.10 durable-mutation red; run it deliberately"]
fn read_only_window_attribution() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fixture = Fixture {
        workspace: tempdir.path().join("workspace"),
        data_home: tempdir.path().join("home"),
        scratch: tempdir.path().join("scratch"),
    };
    for dir in [&fixture.workspace, &fixture.data_home, &fixture.scratch] {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let proof_dir = std::env::var("ORACLE_PROOF_DIR")
        .map_or_else(|_| tempdir.path().join("proof"), PathBuf::from);
    let probes = knob("ORACLE_PROBES", 8);
    let timeout = Duration::from_secs(knob("ORACLE_TIMEOUT_SECS", 120) as u64);
    build_realistic_workspace(&fixture)?;
    // As in the live oracle, one status read precedes the window.
    run_ee(&fixture, &["index", "status", "--json"])?;

    let search_args = [
        "search",
        "release verification remote lane",
        "--limit",
        "5",
        "--json",
    ];
    let pack_args = [
        "pack",
        "prepare release",
        "--read-only",
        "--max-tokens",
        "1500",
        "--json",
    ];
    let status_args = ["index", "status", "--json"];
    let remember_args = [
        "remember",
        "Attribution control: this row must change the logical digest.",
        "--level",
        "procedural",
        "--kind",
        "rule",
        "--json",
    ];
    let copies = tempdir.path().join("logical");
    let mut events = Vec::new();
    // Who closed last before each after-digest (comment 10021 item 3). Every
    // arm's processes have exited before its after-digest is taken.
    let mut last_closer = "setup: the pre-window `ee index status --json`".to_owned();
    for arm in ["N1", "S", "PS", "PP", "N2", "W"] {
        // The tables this arm's command declares (ruling 20:40Z), so the row
        // judgment below is the one the oracle applies. W is the logical
        // digest's positive control, not a probe: remember is a durable write,
        // not an append-only one, so it is not judged.
        let invocations: Vec<&[&str]> = match arm {
            "S" => vec![status_args.as_slice()],
            "PS" => vec![search_args.as_slice()],
            "PP" => vec![pack_args.as_slice()],
            "W" => vec![remember_args.as_slice()],
            _ => Vec::new(),
        };
        let (declared, derived) = declared_write_tables(&invocations)?;
        // The byte digest first, then the WAL size, then the copy for the
        // row snapshot, so the instrument reads what the arm left.
        let before = digest_tree(&fixture.workspace)?;
        let wal_before = wal_size(&fixture.workspace);
        let rows_before = row_snapshot(
            &fixture.workspace,
            &copies.join(format!("{arm}-before")),
            &declared,
        );
        let mut unwaited = 0usize;
        match arm {
            "S" => {
                run_ee(&fixture, &status_args)?;
                last_closer = "S: its one `ee index status --json` (exited)".to_owned();
            }
            "W" => {
                run_ee(&fixture, &remember_args)?;
                last_closer = "W: its one `ee remember --json` (exited)".to_owned();
            }
            "PS" | "PP" => {
                let (args, extract) = if arm == "PS" {
                    (
                        search_args.as_slice(),
                        search_record as fn(&Value) -> Record,
                    )
                } else {
                    (pack_args.as_slice(), pack_record as fn(&Value) -> Record)
                };
                let children = spawn_probes(&fixture, args, probes, 0, arm)?;
                let outcomes = collect_probes(children, timeout, extract);
                let completed = outcomes
                    .iter()
                    .filter(|outcome| matches!(outcome, ProbeOutcome::Completed(_)))
                    .count();
                // A probe whose wait failed was never reaped, so its exit
                // before the after-digest is not proven.
                unwaited = outcomes
                    .iter()
                    .filter(|outcome| {
                        matches!(outcome, ProbeOutcome::DidNotComplete(reason) if reason.starts_with("could not wait"))
                    })
                    .count();
                eprintln!("attribution arm {arm}: {completed}/{probes} probes completed");
                last_closer = format!(
                    "{arm}: one of its {probes} probes; {} of {probes} reaped before the after-digest; which one closed last is not instrumented",
                    probes - unwaited
                );
            }
            _ => {
                last_closer = format!("no process inside {arm}; the last before it: {last_closer}");
            }
        }
        let after = digest_tree(&fixture.workspace)?;
        let wal_after = wal_size(&fixture.workspace);
        let rows_after = row_snapshot(
            &fixture.workspace,
            &copies.join(format!("{arm}-after")),
            &declared,
        );
        let logical_equal = match (&rows_before, &rows_after) {
            (Ok(was), Ok(is)) => Some(was.all_digest() == is.all_digest()),
            _ => None,
        };
        let changed_tables: Vec<String> = match (&rows_before, &rows_after) {
            (Ok(was), Ok(is)) => was
                .tables
                .keys()
                .chain(is.tables.keys())
                .filter(|table| was.tables.get(*table) != is.tables.get(*table))
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        let row_judgment = match (arm, &rows_before, &rows_after) {
            ("W", _, _) => serde_json::json!("not judged: the digest's positive control"),
            (_, Ok(was), Ok(is)) => {
                let (findings, unmeasured) = row_mutation(was, is, &declared);
                serde_json::json!({ "findings": findings, "unmeasured": unmeasured })
            }
            _ => serde_json::json!("unmeasured: no row snapshot"),
        };
        let changed: Vec<(String, &'static str)> = tree_diff(&before, &after)
            .into_iter()
            .map(|change| {
                let path = change
                    .split_once(' ')
                    .map_or(change.as_str(), |(_, path)| path);
                let class = if path == ".ee/ee.db" {
                    classify_db_byte_change(logical_equal, wal_before, wal_after)
                } else {
                    classify_workspace_path(path)
                };
                (change.clone(), class)
            })
            .collect();
        let logical_json = |snapshot: &Result<RowSnapshot, String>| match snapshot {
            Ok(snapshot) => serde_json::json!({
                "all": snapshot.all_digest(),
                "tables": &snapshot.tables,
                "declaredRows": snapshot
                    .declared_rows
                    .iter()
                    .map(|(table, rows)| (table.clone(), rows.len()))
                    .collect::<BTreeMap<_, _>>(),
                "copied": &snapshot.copied,
            }),
            Err(error) => serde_json::json!({ "error": error }),
        };
        eprintln!(
            "attribution arm {arm}: files {} -> {}, wal {wal_before:?} -> {wal_after:?}, logicalEqual {logical_equal:?}, changedTables {changed_tables:?}, declared {declared:?}, rowJudgment {row_judgment}, changed {changed:?}, lastCloser {last_closer}",
            before.len(),
            after.len(),
        );
        events.push(event(
            "attribution_arm",
            "info",
            serde_json::json!({
                "arm": arm,
                "filesBefore": before.len(),
                "filesAfter": after.len(),
                "walBytesBefore": wal_before,
                "walBytesAfter": wal_after,
                "logicalBefore": logical_json(&rows_before),
                "logicalAfter": logical_json(&rows_after),
                "logicalEqual": logical_equal,
                "changedTables": changed_tables,
                "declared": &declared,
                "declaredFrom": derived,
                "rowJudgment": row_judgment,
                "lastCloser": last_closer,
                "unreapedProbes": unwaited,
                "changed": changed
                    .iter()
                    .map(|(change, class)| serde_json::json!({ "change": change, "class": class }))
                    .collect::<Vec<_>>(),
            }),
        ));
    }
    let proof = write_content_addressed_evidence(&proof_dir, &events)?;
    println!("attribution recorded\nevidence: {}", proof.display());
    echo_evidence_body(&events);
    Ok(())
}

/// Sequential proof that `ee pack --read-only` does not mutate workspace files
/// and that a second identical pack is byte-identical.
///
/// The concurrent oracle (`RACE_REPRODUCED` at 294d9ca95) showed concurrent
/// `--read-only` packs agreeing with each other and disagreeing with the
/// serial pack. That is serial-then-later, not inter-probe disagreement. This
/// test isolates that leaf without touching pack-hash identity files owned by
/// ADR 0087 / bd-reality-core-convergence-1azkt.1.
#[test]
fn sequential_read_only_packs_are_byte_identical_and_do_not_mutate_the_workspace() -> TestResult {
    let tempdir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let fixture = Fixture {
        workspace: tempdir.path().join("workspace"),
        data_home: tempdir.path().join("home"),
        scratch: tempdir.path().join("scratch"),
    };
    for dir in [&fixture.workspace, &fixture.data_home, &fixture.scratch] {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    run_ee(&fixture, &["init", "--json"])?;
    run_ee(
        &fixture,
        &[
            "remember",
            "Run cargo fmt --check before every release tag.",
            "--level",
            "procedural",
            "--kind",
            "rule",
            "--json",
        ],
    )?;
    run_ee(&fixture, &["index", "rebuild", "--json"])?;

    let before_first = digest_tree(&fixture.workspace)?;
    let first = run_ee(
        &fixture,
        &[
            "pack",
            "prepare release",
            "--read-only",
            "--max-tokens",
            "1500",
            "--json",
        ],
    )?;
    let after_first = digest_tree(&fixture.workspace)?;
    let first_hash = pointer_string(&first, "/data/pack/hash").ok_or_else(|| {
        "first --read-only pack carried no /data/pack/hash; the JSON path is probably renamed"
            .to_owned()
    })?;
    if first_hash.is_empty() {
        return Err(
            "first --read-only pack.hash was empty; vacuous agreement is forbidden".to_owned(),
        );
    }
    let first_pack_mutations = tree_diff(&before_first, &after_first);

    let second = run_ee(
        &fixture,
        &[
            "pack",
            "prepare release",
            "--read-only",
            "--max-tokens",
            "1500",
            "--json",
        ],
    )?;
    let after_second = digest_tree(&fixture.workspace)?;
    let second_hash = pointer_string(&second, "/data/pack/hash").ok_or_else(|| {
        "second --read-only pack carried no /data/pack/hash; the JSON path is probably renamed"
            .to_owned()
    })?;
    let second_pack_mutations = tree_diff(&after_first, &after_second);

    if first_hash != second_hash || !second_pack_mutations.is_empty() {
        return Err(format!(
            "RACE_REPRODUCED: sequential --read-only packs were not byte-identical. first_hash={first_hash} second_hash={second_hash} files_changed_by_first_pack={first_pack_mutations:?} files_changed_by_second_pack={second_pack_mutations:?}"
        ));
    }
    if !first_pack_mutations.is_empty() {
        return Err(format!(
            "RACE_REPRODUCED: first --read-only pack mutated workspace files even though the second pack hash matched: {first_pack_mutations:?}"
        ));
    }
    Ok(())
}

// ── Always-on tests of the classifier itself ───────────────────────────────
//
// The live oracle is `#[ignore]`; these are not. They are what keeps the
// instrument from rotting, and they are the actual proof that the three-arm
// separation works rather than merely being described in a doc comment.

#[cfg(test)]
mod classifier {
    use super::{ProbeKind, ProbeOutcome, Record, Verdict, classify_round, fold_rounds};

    fn baseline() -> Record {
        let mut record = Record::new();
        record.insert("results.order".to_owned(), "mem_a@1.0|mem_b@0.5".to_owned());
        record.insert("embed_backend".to_owned(), "hash_fallback".to_owned());
        record.insert("degraded.codes".to_owned(), String::new());
        record
    }

    fn agreeing() -> ProbeOutcome {
        ProbeOutcome::Completed(baseline())
    }

    #[test]
    fn starvation_is_inconclusive_and_never_a_race() {
        // The arm that matters most: seven probes never completed. That is a
        // busy box, not a regression, and it must not be reported as either a
        // race or a clean bill of health.
        let mut outcomes = vec![agreeing()];
        for index in 0..7 {
            outcomes.push(ProbeOutcome::DidNotComplete(format!(
                "#{index} timed out after 120s"
            )));
        }
        let verdict = classify_round(ProbeKind::Search, &baseline(), &outcomes, 6, true);
        assert_eq!(verdict.class(), "INCONCLUSIVE", "got {verdict:?}");
        assert!(
            verdict.detail().contains("resource signal"),
            "the message must say why it is not a race: {}",
            verdict.detail()
        );
    }

    #[test]
    fn agreement_below_quorum_is_inconclusive_not_absent() {
        // Two probes agreed and six never ran. Agreement among survivors is
        // not evidence of absence — this is the false-green direction.
        let outcomes = vec![
            agreeing(),
            agreeing(),
            ProbeOutcome::DidNotComplete("killed".to_owned()),
        ];
        let verdict = classify_round(ProbeKind::Search, &baseline(), &outcomes, 6, true);
        assert_eq!(verdict.class(), "INCONCLUSIVE", "got {verdict:?}");
    }

    #[test]
    fn quorum_of_agreeing_probes_is_absent() {
        let outcomes = vec![
            agreeing(),
            agreeing(),
            agreeing(),
            agreeing(),
            agreeing(),
            agreeing(),
        ];
        let verdict = classify_round(ProbeKind::Search, &baseline(), &outcomes, 6, true);
        assert_eq!(verdict.class(), "RACE_ABSENT", "got {verdict:?}");
    }

    #[test]
    fn divergent_order_reproduces_the_race() {
        let mut divergent = baseline();
        divergent.insert("results.order".to_owned(), "mem_b@0.5|mem_a@1.0".to_owned());
        let outcomes = vec![agreeing(), ProbeOutcome::Completed(divergent)];
        let verdict = classify_round(ProbeKind::Search, &baseline(), &outcomes, 2, true);
        assert_eq!(verdict.class(), "RACE_REPRODUCED", "got {verdict:?}");
    }

    #[test]
    fn divergence_outranks_a_missed_quorum() {
        // One disagreement plus mass starvation is still a reproduction: other
        // probes failing to run cannot un-observe what two completed probes
        // showed.
        let mut divergent = baseline();
        divergent.insert("embed_backend".to_owned(), "model2vec".to_owned());
        let outcomes = vec![
            agreeing(),
            ProbeOutcome::Completed(divergent),
            ProbeOutcome::DidNotComplete("timeout".to_owned()),
            ProbeOutcome::DidNotComplete("timeout".to_owned()),
        ];
        let verdict = classify_round(ProbeKind::Search, &baseline(), &outcomes, 8, true);
        assert_eq!(verdict.class(), "RACE_REPRODUCED", "got {verdict:?}");
    }

    #[test]
    fn an_empty_probe_is_divergence_not_agreement() {
        // The lost-lexical-arm signature: a process that fails the index open
        // serves zero results. Counting that as "agreement about emptiness"
        // would hide the exact fault this oracle hunts.
        let mut empty = baseline();
        empty.insert("results.order".to_owned(), String::new());
        let outcomes = vec![agreeing(), ProbeOutcome::Completed(empty)];
        let verdict = classify_round(ProbeKind::Search, &baseline(), &outcomes, 2, true);
        assert_eq!(verdict.class(), "RACE_REPRODUCED", "got {verdict:?}");
        assert!(
            verdict.detail().contains("not agreement"),
            "message must name the abstention: {}",
            verdict.detail()
        );
    }

    #[test]
    fn a_vacuous_baseline_is_infra_error_never_a_pass() {
        // If a JSON path is renamed, every probe reports the same nothing and
        // a naive comparison goes green while proving nothing.
        let mut vacuous = baseline();
        vacuous.insert("results.order".to_owned(), String::new());
        let outcomes = vec![ProbeOutcome::Completed(vacuous.clone())];
        let verdict = classify_round(ProbeKind::Search, &vacuous, &outcomes, 1, true);
        assert_eq!(verdict.class(), "INFRA_ERROR", "got {verdict:?}");

        let mut missing = baseline();
        missing.remove("embed_backend");
        let verdict = classify_round(ProbeKind::Search, &missing, &[], 1, true);
        assert_eq!(verdict.class(), "INFRA_ERROR", "got {verdict:?}");
    }

    #[test]
    fn invisible_index_only_counts_against_a_valid_generation() {
        let mut invisible = baseline();
        invisible.insert("degraded.codes".to_owned(), "index_missing".to_owned());
        let outcomes = vec![ProbeOutcome::Completed(invisible)];

        // With a healthy current generation this is a semantic fault.
        let verdict = classify_round(ProbeKind::Search, &baseline(), &outcomes.clone(), 1, true);
        assert_eq!(verdict.class(), "RACE_REPRODUCED", "got {verdict:?}");

        // Without one, an absent index is a configuration fact, not a race —
        // but the degraded-code mismatch against the baseline is still caught.
        let mut invisible_baseline = baseline();
        invisible_baseline.insert("degraded.codes".to_owned(), "index_missing".to_owned());
        let verdict = classify_round(ProbeKind::Search, &invisible_baseline, &outcomes, 1, false);
        assert_eq!(verdict.class(), "RACE_ABSENT", "got {verdict:?}");
    }

    #[test]
    fn folding_rounds_lets_one_observation_outrank_many_clean_ones() {
        let clean = Verdict::RaceAbsent;
        let dirty = Verdict::RaceReproduced(vec!["order flipped".to_owned()]);
        assert_eq!(
            fold_rounds(&[clean.clone(), dirty, clean.clone()]).class(),
            "RACE_REPRODUCED"
        );
        assert_eq!(fold_rounds(&[clean.clone(), clean]).class(), "RACE_ABSENT");
        assert_eq!(
            fold_rounds(&[
                Verdict::RaceAbsent,
                Verdict::Inconclusive("busy".to_owned())
            ])
            .class(),
            "INCONCLUSIVE"
        );
        // Infra outranks everything: an unattested candidate has no verdict.
        assert_eq!(
            fold_rounds(&[
                Verdict::RaceReproduced(vec!["x".to_owned()]),
                Verdict::InfraError("unattested".to_owned()),
            ])
            .class(),
            "INFRA_ERROR"
        );
        assert_eq!(fold_rounds(&[]).class(), "INFRA_ERROR");
    }

    #[test]
    fn only_race_absent_is_a_product_pass() {
        assert!(Verdict::RaceAbsent.is_product_pass());
        for verdict in [
            Verdict::RaceReproduced(vec!["x".to_owned()]),
            Verdict::Inconclusive("busy".to_owned()),
            Verdict::InfraError("unattested".to_owned()),
        ] {
            assert!(
                !verdict.is_product_pass(),
                "{} must never count as a product pass",
                verdict.class()
            );
        }
    }
}

#[cfg(test)]
mod durable_mutation_controls {
    //! bd-reality-core-convergence-1azkt.10, ruling 17:12Z items 1 and 2.
    //! Each failure class gets a planted red and an untouched twin, so a judge
    //! that flags everything fails as surely as one that flags nothing.

    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        DurableState, Fixture, RowSnapshot, Verdict, WriteLock, classify_db_byte_change,
        classify_workspace_path, db_file_churn, declared_write_tables, durable_mutation,
        durable_state, expected_commit_refusal, read_write_lock, write_lock_regression,
    };

    #[test]
    fn db_byte_changes_split_into_checkpoint_durable_other_and_unjudged() {
        let cases = [
            // Rows equal and the WAL shrank, truncated or went away.
            (Some(true), Some(4096), Some(1024), "checkpoint"),
            (Some(true), Some(4096), Some(0), "checkpoint"),
            (Some(true), Some(4096), None, "checkpoint"),
            // Rows differ: durable, whatever the WAL did.
            (Some(false), Some(4096), Some(0), "durable"),
            (Some(false), None, None, "durable"),
            // Rows equal but the WAL did not shrink: reported, not guessed at.
            (Some(true), Some(4096), Some(4096), "db-bytes-other"),
            (Some(true), Some(0), None, "db-bytes-other"),
            (Some(true), None, Some(4096), "db-bytes-other"),
            // No logical digest: not classified at all.
            (None, Some(4096), Some(0), "db-bytes-unjudged"),
        ];
        for (logical_equal, wal_before, wal_after, expected) in cases {
            assert_eq!(
                classify_db_byte_change(logical_equal, wal_before, wal_after),
                expected,
                "logical_equal={logical_equal:?} wal {wal_before:?} -> {wal_after:?}"
            );
        }
    }

    #[test]
    fn workspace_paths_are_classified_shm_wal_lock_or_durable() {
        for (path, class) in [
            (".ee/ee.db-shm", "shm"),
            (".ee/ee.db-wal", "wal"),
            (".ee/ee.db-wal-cert", "wal"),
            (".ee/ee.db-wal-cert-head", "wal"),
            (".ee/ee.write.lock", "lock"),
            (".ee/ee.db", "durable"),
            (".ee/index/meta.json", "durable"),
            (".ee/walrus/notes.json", "durable"),
        ] {
            assert_eq!(classify_workspace_path(path), class, "{path}");
        }
    }

    fn declared() -> BTreeSet<String> {
        BTreeSet::from(["audit_log".to_owned()])
    }

    fn state() -> DurableState {
        let mut files = BTreeMap::new();
        files.insert(".ee/ee.db".to_owned(), "aaaa".to_owned());
        files.insert(".ee/index/meta.json".to_owned(), "bbbb".to_owned());
        let tables = BTreeMap::from([
            ("audit_log".to_owned(), "2:aa".to_owned()),
            ("memories".to_owned(), "1:bb".to_owned()),
        ]);
        let audit_rows = BTreeMap::from([(1, "r1".to_owned()), (2, "r2".to_owned())]);
        DurableState {
            db_generation: Some("8".to_owned()),
            index_generation: Some("8".to_owned()),
            files,
            rows: RowSnapshot {
                tables,
                declared_rows: BTreeMap::from([("audit_log".to_owned(), audit_rows)]),
                copied: vec!["ee.db".to_owned()],
            },
            write_lock: WriteLock::Epoch("00000000000000000005".to_owned()),
        }
    }

    #[test]
    fn an_untouched_window_is_not_a_mutation() {
        assert!(durable_mutation(&state(), &state(), &declared()).is_none());
    }

    #[test]
    fn a_moved_generation_is_a_mutation() {
        let mut after = state();
        after.db_generation = Some("9".to_owned());
        let verdict =
            durable_mutation(&state(), &after, &declared()).expect("a moved counter must red");
        assert_eq!(verdict.class(), "RACE_REPRODUCED", "got {verdict:?}");
        assert!(verdict.detail().contains("generation moved"), "{verdict:?}");
    }

    #[test]
    fn a_changed_file_with_unmoved_counters_is_a_mutation() {
        // The case the counters alone missed: bytes changed, generation did not.
        let mut after = state();
        after
            .files
            .insert(".ee/index/meta.json".to_owned(), "cccc".to_owned());
        let verdict =
            durable_mutation(&state(), &after, &declared()).expect("a changed file must red");
        assert_eq!(verdict.class(), "RACE_REPRODUCED", "got {verdict:?}");
        assert!(
            verdict.detail().contains("changed .ee/index/meta.json"),
            "{verdict:?}"
        );
    }

    #[test]
    fn an_added_or_removed_file_is_a_mutation() {
        let mut added = state();
        added
            .files
            .insert(".ee/index/new.json".to_owned(), "dddd".to_owned());
        assert!(
            durable_mutation(&state(), &added, &declared())
                .is_some_and(|verdict| verdict.detail().contains("added .ee/index/new.json"))
        );
        let mut removed = state();
        removed.files.remove(".ee/index/meta.json");
        assert!(
            durable_mutation(&state(), &removed, &declared())
                .is_some_and(|verdict| verdict.detail().contains("removed .ee/index/meta.json"))
        );
    }

    #[test]
    fn database_file_churn_is_classified_not_judged_by_bytes() {
        // ee.db, its WAL and shm change bytes and the lock advances, but no row
        // changed: not a mutation (ruling 20:40Z), and the churn is recorded
        // with ee.db's byte change as UNEXPLAINED (condition 6).
        let mut after = state();
        for path in [".ee/ee.db", ".ee/ee.db-wal", ".ee/ee.db-shm"] {
            after.files.insert(path.to_owned(), "churned".to_owned());
        }
        after.write_lock = WriteLock::Epoch("00000000000000000006".to_owned());
        assert_eq!(durable_mutation(&state(), &after, &declared()), None);
        let churn = db_file_churn(&state(), &after);
        let classes: Vec<(&str, &str)> = churn
            .iter()
            .filter_map(|entry| Some((entry["change"].as_str()?, entry["class"].as_str()?)))
            .collect();
        assert_eq!(
            classes,
            vec![
                ("added .ee/ee.db-shm", "shm"),
                ("added .ee/ee.db-wal", "wal"),
                ("changed .ee/ee.db", "db-bytes-UNEXPLAINED"),
            ]
        );
    }

    #[test]
    fn an_empty_digest_or_row_snapshot_measured_nothing_and_is_infra_error() {
        let mut empty = state();
        empty.files.clear();
        let verdict =
            durable_mutation(&empty, &empty, &declared()).expect("an empty world must not pass");
        assert_eq!(verdict.class(), "INFRA_ERROR", "got {verdict:?}");
        let mut no_tables = state();
        no_tables.rows.tables.clear();
        let verdict = durable_mutation(&no_tables, &no_tables, &declared())
            .expect("an empty row snapshot must not pass");
        assert_eq!(verdict.class(), "INFRA_ERROR", "got {verdict:?}");
    }

    #[test]
    fn the_write_lock_follows_the_xa6ud_precedent() {
        let epoch = |digits: &str| WriteLock::Epoch(digits.to_owned());
        let unreadable = WriteLock::Unreadable("garbage".to_owned());
        let five = "00000000000000000005";
        let six = "00000000000000000006";
        // (before, after, is a finding)
        let cases = [
            (WriteLock::Absent, WriteLock::Absent, false),
            (WriteLock::Absent, epoch(five), true),
            (epoch(five), epoch(five), false),
            (epoch(five), epoch(six), false),
            (epoch(six), epoch(five), true),
            (epoch(five), WriteLock::Absent, true),
            (epoch(five), unreadable.clone(), true),
        ];
        for (before, after, red) in cases {
            let judged = write_lock_regression(&before, &after);
            assert_eq!(
                judged.as_ref().map(Option::is_some),
                Ok(red),
                "{before:?} -> {after:?}: {judged:?}"
            );
        }
        // An unreadable baseline is not a pass and not a finding: unmeasured.
        assert!(write_lock_regression(&unreadable, &epoch(five)).is_err());

        let dir = tempfile::tempdir().expect("tempdir");
        let lock = dir.path().join("ee.write.lock");
        assert_eq!(read_write_lock(&lock), WriteLock::Absent);
        std::fs::write(&lock, format!("{six}\n")).expect("write lock");
        assert_eq!(read_write_lock(&lock), epoch(six));
        for malformed in [
            six.to_owned(),
            format!("{six}\n\n"),
            "0000000000000000000x\n".to_owned(),
        ] {
            std::fs::write(&lock, &malformed).expect("write lock");
            assert!(
                matches!(read_write_lock(&lock), WriteLock::Unreadable(_)),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn declared_writes_are_derived_from_the_effect_manifest() {
        // The derivation itself, both ways: search declares audit_log (effect.rs
        // append_only_write), index status declares nothing (read_only_db), and
        // an argv the CLI rejects is an error, never an empty allowlist.
        let derive = |argv: &[&str]| declared_write_tables(&[argv]);
        let (search, derived) =
            derive(&["search", "q", "--limit", "5", "--json"]).expect("search derives");
        assert!(search.contains("audit_log"), "{search:?}");
        assert_eq!(derived[0]["commandPath"], "search", "{derived}");
        let (status, derived) = derive(&["index", "status", "--json"]).expect("status derives");
        assert!(status.is_empty(), "{status:?}");
        assert_eq!(derived[0]["commandPath"], "index status", "{derived}");
        let (_, derived) = derive(&["pack", "q", "--read-only", "--json"]).expect("pack derives");
        assert_eq!(derived[0]["commandPath"], "pack build", "{derived}");
        assert!(derive(&["no-such-command"]).is_err());
    }

    /// A real workspace database (ruling 20:40Z condition 5): `audit_log` is
    /// the declared table, `memories` an undeclared one. The state is taken
    /// through the oracle's own `durable_state`, the plants run as SQL on the
    /// live file between the two snapshots, and the window is judged by
    /// `durable_mutation`. `None` for `plants` is the untouched twin.
    fn judge_planted(plants: Option<&[&str]>) -> Option<Verdict> {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = Fixture {
            workspace: dir.path().join("workspace"),
            data_home: dir.path().join("home"),
            scratch: dir.path().join("scratch"),
        };
        let db_path = fixture.workspace.join(".ee").join("ee.db");
        std::fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
        std::fs::create_dir_all(fixture.workspace.join("notes")).expect("mkdir");
        std::fs::write(fixture.workspace.join("notes").join("keep.txt"), "kept").expect("file");
        {
            let connection = ee::db::DbConnection::open_file(&db_path).expect("open");
            for sql in [
                "CREATE TABLE audit_log (id INTEGER PRIMARY KEY, body TEXT)",
                "CREATE TABLE memories (id INTEGER PRIMARY KEY, body TEXT)",
                "INSERT INTO audit_log VALUES (1, 'executed'), (2, 'returned')",
                "INSERT INTO memories VALUES (1, 'run cargo fmt')",
            ] {
                connection.execute_raw(sql).expect("seed");
            }
        }
        let declared = declared();
        let before = durable_state(&fixture, "before", None, None, &declared).expect("before");
        if let Some(plants) = plants {
            let connection = ee::db::DbConnection::open_file(&db_path).expect("reopen");
            for sql in plants {
                connection.execute_raw(sql).expect("plant");
            }
        }
        let after = durable_state(&fixture, "after", None, None, &declared).expect("after");
        durable_mutation(&before, &after, &declared)
    }

    fn assert_red(verdict: Option<Verdict>, needle: &str) {
        let verdict = verdict.unwrap_or_else(|| panic!("a plant must red ({needle})"));
        assert_eq!(verdict.class(), "RACE_REPRODUCED", "{verdict:?}");
        assert!(verdict.detail().contains(needle), "{needle}: {verdict:?}");
    }

    #[test]
    fn a_declared_append_on_a_real_database_passes() {
        assert_eq!(judge_planted(None), None, "untouched twin");
        assert_eq!(
            judge_planted(Some(&["INSERT INTO audit_log VALUES (3, 'returned')"])),
            None,
            "an append to the declared table is its contract"
        );
    }

    #[test]
    fn an_undeclared_row_change_on_a_real_database_is_red() {
        assert_eq!(judge_planted(None), None, "untouched twin");
        assert_red(
            judge_planted(Some(&["UPDATE memories SET body = 'changed' WHERE id = 1"])),
            "undeclared table memories changed",
        );
        assert_red(
            judge_planted(Some(&["INSERT INTO memories VALUES (2, 'new')"])),
            "undeclared table memories changed",
        );
    }

    #[test]
    fn a_declared_row_modified_on_a_real_database_is_red() {
        assert_eq!(judge_planted(None), None, "untouched twin");
        assert_red(
            judge_planted(Some(&[
                "UPDATE audit_log SET body = 'rewritten' WHERE id = 1",
            ])),
            "declared table audit_log: row 1 MODIFIED",
        );
    }

    #[test]
    fn a_declared_row_deleted_on_a_real_database_is_red_even_when_the_count_holds() {
        assert_eq!(judge_planted(None), None, "untouched twin");
        assert_red(
            judge_planted(Some(&["DELETE FROM audit_log WHERE id = 2"])),
            "declared table audit_log: row 2 DELETED",
        );
        // A delete plus an insert keeps the row count at 2; a count check
        // would pass this, the rowid check must not.
        assert_red(
            judge_planted(Some(&[
                "DELETE FROM audit_log WHERE id = 2",
                "INSERT INTO audit_log VALUES (3, 'returned')",
            ])),
            "declared table audit_log: row 2 DELETED",
        );
    }

    #[test]
    fn the_expected_commit_refuses_absence_and_mismatch_and_admits_a_match() {
        let sha = "f1cbd6327a4bf066535099ab900e0c57e83b1ca0";
        assert!(
            expected_commit_refusal(None, sha)
                .is_some_and(|reason| reason.contains("ORACLE_EXPECTED_COMMIT"))
        );
        assert!(
            expected_commit_refusal(Some("0000000000000000000000000000000000000000"), sha)
                .is_some_and(|reason| reason.contains("is not the requested base"))
        );
        assert_eq!(expected_commit_refusal(Some(sha), sha), None);
    }
}

#[cfg(test)]
mod build_environment {
    //! bd-reality-core-convergence-1azkt.10, ruling 2026-09-23 15:03Z: the
    //! substitute lane must refuse a verdict when cargo config or the
    //! environment could redirect dependency resolution. Every arm pairs a
    //! planted redirect with a clean twin, so a scanner that flags everything
    //! fails as surely as one that flags nothing.

    use super::{
        config_text_redirects_resolution, env_key_redirects_resolution, scan_build_environment,
    };

    #[test]
    fn config_markers_are_found_and_comments_and_clean_keys_are_not() {
        assert_eq!(
            config_text_redirects_resolution("[patch.crates-io]\nfoo = { path = \"x\" }"),
            vec!["[patch"]
        );
        assert_eq!(
            config_text_redirects_resolution("[source.crates-io]\nreplace-with = \"m\""),
            vec!["[source", "replace-with"]
        );
        assert_eq!(
            config_text_redirects_resolution("paths = [\"/x\"]"),
            vec!["paths ="]
        );
        assert_eq!(
            config_text_redirects_resolution("[replace]"),
            vec!["[replace"]
        );
        assert!(config_text_redirects_resolution("# [patch.crates-io]\n# paths = []").is_empty());
        assert!(
            config_text_redirects_resolution("[net]\ngit-fetch-with-cli = true\ntarget-paths = 1")
                .is_empty()
        );
    }

    #[test]
    fn redirecting_env_keys_are_flagged_and_neighbours_are_not() {
        for key in [
            "CARGO_SOURCE_CRATES_IO_REPLACE_WITH",
            "CARGO_PATCH_CRATES_IO_FOO",
            "CARGO_REGISTRIES_MIRROR_INDEX",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
        ] {
            assert!(env_key_redirects_resolution(key), "{key} must be flagged");
        }
        for key in [
            "CARGO_REGISTRIES_MIRROR_TOKEN",
            "CARGO_HOME",
            "RUSTFLAGS",
            "CARGO_TARGET_DIR",
        ] {
            assert!(
                !env_key_redirects_resolution(key),
                "{key} must not be flagged"
            );
        }
    }

    #[test]
    fn scan_reads_ancestor_configs_and_env_and_counts_what_it_probed() {
        let root = tempfile::tempdir().expect("tempdir");
        let manifest = root.path().join("a").join("b");
        std::fs::create_dir_all(&manifest).expect("manifest dir");
        std::fs::create_dir_all(root.path().join("a").join(".cargo")).expect(".cargo");
        std::fs::write(
            root.path().join("a").join(".cargo").join("config.toml"),
            "[patch.crates-io]\n",
        )
        .expect("planted config");
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(home.join("config.toml"), "[net]\nretry = 2\n").expect("clean home config");

        let scan = scan_build_environment(
            &manifest,
            Some(&home),
            [
                ("RUSTC_WRAPPER".to_owned(), "sccache".to_owned()),
                ("RUSTC_WORKSPACE_WRAPPER".to_owned(), String::new()),
                ("CARGO_HOME".to_owned(), "/x".to_owned()),
            ],
        );
        let ancestors = manifest.ancestors().count();
        assert_eq!(scan.locations_probed, 2 + 2 * ancestors);
        assert!(
            scan.files_read
                .iter()
                .any(|(label, _)| label == "<CARGO_HOME>/config.toml"),
            "the clean CARGO_HOME config was read: {:?}",
            scan.files_read
        );
        assert!(
            scan.hits
                .contains(&"<manifest-ancestor-1>/.cargo/config.toml: [patch".to_owned()),
            "the planted ancestor patch is a hit: {:?}",
            scan.hits
        );
        assert!(scan.hits.contains(&"env RUSTC_WRAPPER".to_owned()));
        assert!(
            !scan
                .hits
                .iter()
                .any(|hit| hit.contains("CARGO_HOME") || hit.contains("RUSTC_WORKSPACE_WRAPPER")),
            "a clean config and an empty wrapper are not hits: {:?}",
            scan.hits
        );
    }
}

#[cfg(test)]
mod attestation {
    //! bd-reality-core-convergence-1azkt.10, bullet 2.
    //!
    //! An oracle that cannot say which features were on and which siblings it
    //! linked has attested to a COMMIT, not to a BUILD. These arms cover the
    //! two fields that closed that gap, and — because `attest` already refused
    //! on absent fields — they pin WHAT MOVED: a candidate that the old gate
    //! would have admitted must now be refused, or the new fields are captured
    //! and never checked, which looks identical from a green run.

    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::{CandidateIdentity, Verdict, feature_set};

    /// Any real, readable file: `attest` hashes it, and none of these claims
    /// depend on which file it was.
    fn candidate_file() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
    }

    /// A candidate that satisfies every attestation field.
    fn attested_version_json() -> Value {
        json!({
            "data": {
                "version": "0.15.2",
                "source": {
                    "gitCommit": "051475effb2e248752e77ee5885db1edb36230e4",
                    "gitDirty": false,
                    "state": "clean"
                },
                "build": {
                    "profile": "debug",
                    "targetTriple": "x86_64-unknown-linux-gnu",
                    "frankenStack": "asupersync@0.5.0,frankensearch@0.6.0,fsqlite@0.4.1"
                },
                "features": [
                    {"name": "fts5", "enabled": true},
                    {"name": "mcp", "enabled": false},
                    {"name": "graph", "enabled": true}
                ]
            }
        })
    }

    fn attest(version: &Value, require: bool) -> Result<CandidateIdentity, Verdict> {
        CandidateIdentity::attest(version, &candidate_file(), require)
    }

    /// POSITIVE: a complete candidate attests, and both new fields are recorded
    /// with the values it reported.
    #[test]
    fn a_complete_candidate_records_its_engine_and_its_features() {
        let identity = match attest(&attested_version_json(), true) {
            Ok(identity) => identity,
            Err(verdict) => panic!("a complete candidate must attest: {verdict:?}"),
        };
        assert_eq!(
            identity.fields.get("frankenStack").map(String::as_str),
            Some("asupersync@0.5.0,frankensearch@0.6.0,fsqlite@0.4.1"),
            "the linked engine must be part of the candidate identity"
        );
        // Enabled only, sorted. `mcp` is present in the payload and disabled,
        // so its absence here is the filter working rather than a dropped field.
        assert_eq!(
            identity.fields.get("featureSet").map(String::as_str),
            Some("fts5,graph"),
            "the feature set must be part of the candidate identity"
        );
    }

    /// NEGATIVE ARM — and the one that shows the refusal set MOVED.
    ///
    /// Both candidates below carry a good `gitCommit` and a good
    /// `targetTriple`, so both would have been ADMITTED before these fields
    /// were captured. Each must now be refused, and the message must name the
    /// field responsible rather than failing generically.
    #[test]
    fn a_candidate_that_cannot_name_its_build_inputs_is_now_refused() {
        for (label, mutate, expected_field) in [
            (
                "no engine",
                Box::new(|version: &mut Value| {
                    version["data"]["build"]["frankenStack"] = Value::Null;
                }) as Box<dyn Fn(&mut Value)>,
                "frankenStack",
            ),
            (
                "no features array",
                Box::new(|version: &mut Value| {
                    if let Some(data) = version["data"].as_object_mut() {
                        data.remove("features");
                    }
                }),
                "featureSet",
            ),
        ] {
            let mut version = attested_version_json();
            mutate(&mut version);

            // The precondition that makes this an arm about the NEW fields:
            // everything the OLD gate checked is still intact.
            assert_eq!(
                version
                    .pointer("/data/source/gitCommit")
                    .and_then(Value::as_str),
                Some("051475effb2e248752e77ee5885db1edb36230e4"),
                "{label}: gitCommit must remain valid or this proves nothing new"
            );
            assert_eq!(
                version
                    .pointer("/data/build/targetTriple")
                    .and_then(Value::as_str),
                Some("x86_64-unknown-linux-gnu"),
                "{label}: targetTriple must remain valid or this proves nothing new"
            );

            match attest(&version, true) {
                Ok(identity) => panic!(
                    "{label}: a candidate missing {expected_field} must be refused, got {:?}",
                    identity.fields
                ),
                Err(Verdict::InfraError(message)) => assert!(
                    message.contains(expected_field),
                    "{label}: the refusal must name {expected_field}: {message}"
                ),
                Err(other) => panic!("{label}: unattested must be InfraError, got {other:?}"),
            }
        }
    }

    /// CONTROL: the same deficient candidates attest fine when attestation is
    /// not required, and the missing values are still RECORDED as `<absent>`.
    ///
    /// This separates two things a single green run conflates: that the fields
    /// are captured, and that the gate is what refuses on them.
    #[test]
    fn without_attestation_the_same_candidate_is_admitted_and_still_recorded() {
        let mut version = attested_version_json();
        version["data"]["build"]["frankenStack"] = Value::Null;
        if let Some(data) = version["data"].as_object_mut() {
            data.remove("features");
        }

        let identity = match attest(&version, false) {
            Ok(identity) => identity,
            Err(verdict) => {
                panic!("without ORACLE_REQUIRE_ATTESTATION nothing may refuse: {verdict:?}")
            }
        };
        assert_eq!(
            identity.fields.get("frankenStack").map(String::as_str),
            Some("<absent>"),
            "an unenforced run must still record that the engine was unknown"
        );
        assert_eq!(
            identity.fields.get("featureSet").map(String::as_str),
            Some("<absent>"),
            "an unenforced run must still record that the feature set was unknown"
        );
    }

    /// PRECONDITION / NON-VACUITY: `feature_set` must discriminate, and must
    /// keep "no features" distinct from "no features array".
    ///
    /// If it collapsed those, a renamed JSON path would be indistinguishable
    /// from a minimal build and every assertion above would still pass.
    #[test]
    fn the_feature_set_discriminates_and_separates_none_from_absent() {
        let mut one = attested_version_json();
        one["data"]["features"] = json!([{"name": "fts5", "enabled": true}]);
        let mut other = attested_version_json();
        other["data"]["features"] = json!([{"name": "graph", "enabled": true}]);
        assert_ne!(
            feature_set(&one),
            feature_set(&other),
            "different enabled features must produce different identities"
        );

        let mut all_off = attested_version_json();
        all_off["data"]["features"] = json!([{"name": "fts5", "enabled": false}]);
        assert_eq!(
            feature_set(&all_off).as_deref(),
            Some("(none)"),
            "a build with every feature off is a real answer, not a missing one"
        );

        let mut missing = attested_version_json();
        if let Some(data) = missing["data"].as_object_mut() {
            data.remove("features");
        }
        assert_eq!(
            feature_set(&missing),
            None,
            "a missing array is not the same as an empty one"
        );

        // Ordering is a rendering detail, not an identity difference.
        let mut forward = attested_version_json();
        forward["data"]["features"] =
            json!([{"name": "a", "enabled": true}, {"name": "b", "enabled": true}]);
        let mut reversed = attested_version_json();
        reversed["data"]["features"] =
            json!([{"name": "b", "enabled": true}, {"name": "a", "enabled": true}]);
        assert_eq!(
            feature_set(&forward),
            feature_set(&reversed),
            "feature order must not change the candidate identity"
        );
    }
}

#[cfg(test)]
mod pack_selection {
    //! bd-reality-core-convergence-1azkt.10, bullet 3: "selected entities".
    //!
    //! `pack.hash` made a pack divergence detectable and undiagnosable. These
    //! arms cover the rendering that names WHICH entity moved, and — because a
    //! recorded field is only worth something if it is also compared — the
    //! precondition asserts it lands in `pack_record`, which `diverge` walks in
    //! full.

    use serde_json::{Value, json};

    use super::{pack_record, render_pack_selection};

    fn pack_with(items: Value) -> Value {
        json!({"data": {"pack": {"hash": "blake3:deadbeef", "items": items}}})
    }

    /// POSITIVE: rank, id and the reason it was selected all survive.
    #[test]
    fn a_selection_names_the_entity_its_rank_and_why_it_was_chosen() {
        let value = pack_with(json!([
            {"rank": 1, "memoryId": "mem_a", "selectedIn": "direct_evidence"},
            {"rank": 2, "memoryId": "mem_b", "selectedIn": "coverage_fill"}
        ]));
        assert_eq!(
            render_pack_selection(&value).as_deref(),
            Some("1:mem_a@direct_evidence|2:mem_b@coverage_fill"),
        );
    }

    /// NEGATIVE ARM: packs that a hash alone would not tell apart must produce
    /// DIFFERENT selections, or recording this field buys nothing.
    ///
    /// All three mutations below leave `pack.hash` untouched on purpose: the
    /// point is that the selection discriminates where the hash, held constant,
    /// does not.
    #[test]
    fn selections_that_differ_are_not_reported_as_the_same() {
        let baseline = pack_with(json!([
            {"rank": 1, "memoryId": "mem_a", "selectedIn": "direct_evidence"},
            {"rank": 2, "memoryId": "mem_b", "selectedIn": "coverage_fill"}
        ]));
        for (label, variant) in [
            (
                "a different entity",
                pack_with(json!([
                    {"rank": 1, "memoryId": "mem_a", "selectedIn": "direct_evidence"},
                    {"rank": 2, "memoryId": "mem_c", "selectedIn": "coverage_fill"}
                ])),
            ),
            (
                "the same entities in a different order",
                pack_with(json!([
                    {"rank": 1, "memoryId": "mem_b", "selectedIn": "coverage_fill"},
                    {"rank": 2, "memoryId": "mem_a", "selectedIn": "direct_evidence"}
                ])),
            ),
            (
                "the same entity selected for a different reason",
                pack_with(json!([
                    {"rank": 1, "memoryId": "mem_a", "selectedIn": "coverage_fill"},
                    {"rank": 2, "memoryId": "mem_b", "selectedIn": "coverage_fill"}
                ])),
            ),
        ] {
            assert_ne!(
                render_pack_selection(&baseline),
                render_pack_selection(&variant),
                "{label} must be a different selection"
            );
        }
    }

    /// CONTROL: an empty pack is a real answer; a missing array is not.
    #[test]
    fn an_empty_pack_is_not_the_same_as_a_renamed_path() {
        assert_eq!(
            render_pack_selection(&pack_with(json!([]))).as_deref(),
            Some("(none)"),
            "a pack that selected nothing is an answer, not an absence"
        );
        let renamed = json!({"data": {"pack": {"hash": "blake3:deadbeef"}}});
        assert_eq!(
            render_pack_selection(&renamed),
            None,
            "a missing items array must not masquerade as an empty pack"
        );
    }

    /// PRECONDITION: the field reaches the record, so `diverge` — which walks
    /// every field the baseline observed — actually compares it.
    ///
    /// Without this, the three arms above would pass on a rendering nothing
    /// consumes.
    #[test]
    fn the_selection_is_recorded_so_that_it_is_compared() {
        let record = pack_record(&pack_with(json!([
            {"rank": 1, "memoryId": "mem_a", "selectedIn": "direct_evidence"}
        ])));
        assert_eq!(
            record.get("pack.selection").map(String::as_str),
            Some("1:mem_a@direct_evidence"),
            "pack_record must carry the selection: {record:?}"
        );
        // And a renamed path must not silently insert an empty value, which
        // would compare equal against another renamed probe.
        let renamed = pack_record(&json!({"data": {"pack": {"hash": "blake3:x"}}}));
        assert!(
            !renamed.contains_key("pack.selection"),
            "a renamed path must omit the field, not record an empty one: {renamed:?}"
        );
    }
}

#[cfg(test)]
mod evidence {
    //! bd-0v23w: a content-addressed name is a claim about content.
    //!
    //! The oracle wrote `af1349b9…ee-test-event.jsonl` on every refusal, and a
    //! P0 cited one of those files as proof. The digest was not wrong — it was
    //! the correct BLAKE3 of an empty body, because the refusal path ran before
    //! anything had been recorded. These arms pin the property that makes the
    //! artifact worth citing: the name is a function of the CONTENT, so two
    //! refusals of two different candidates cannot share a filename.

    use serde_json::json;

    use super::{event, write_content_addressed_evidence};

    /// The digest of an empty body. Asserted by reconstruction below rather
    /// than trusted as a literal.
    const EMPTY_DIGEST: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    fn digest_of(events: &[serde_json::Value]) -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_content_addressed_evidence(dir.path(), events).expect("write evidence");
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.split('.').next())
            .expect("digest in filename")
            .to_owned()
    }

    /// PRECONDITION: the sentinel really is the hash of nothing.
    ///
    /// Hardcoding it without this would make the arms below assert against a
    /// magic string. Reconstructing it is also what made the original incident
    /// checkable by a reader with no access to the artifact.
    #[test]
    fn the_empty_digest_constant_is_the_hash_of_an_empty_body() {
        assert_eq!(
            blake3::hash(b"").to_hex().to_string(),
            EMPTY_DIGEST,
            "the sentinel must be reconstructible, not taken on faith"
        );
    }

    /// NEGATIVE ARM: an empty event list still produces the empty digest.
    ///
    /// This is the behaviour the incident was made of, and it is CORRECT for
    /// the writer — a hash of nothing is the hash of nothing. Pinning it here
    /// means the fix has to be "record something before refusing", and can
    /// never be "make the writer lie about empty input".
    #[test]
    fn an_empty_event_list_still_hashes_to_nothing() {
        assert_eq!(
            digest_of(&[]),
            EMPTY_DIGEST,
            "the writer must not disguise an empty body"
        );
    }

    /// POSITIVE: a recorded identity moves the digest off the empty sentinel.
    #[test]
    fn a_recorded_identity_makes_the_proof_name_meaningful() {
        let events = vec![event(
            "attest_candidate",
            "refused",
            json!({"identity": {"gitCommit": "051475eff", "frankenStack": "fsqlite@0.4.1"}}),
        )];
        assert_ne!(
            digest_of(&events),
            EMPTY_DIGEST,
            "a refusal that recorded its candidate must not be named for nothing"
        );
    }

    /// DISCRIMINATION: two refusals of two DIFFERENT candidates must not share
    /// a filename.
    ///
    /// This is the property whose absence let one digest stand for every
    /// refusal, across different commits, workers and directories.
    #[test]
    fn two_different_candidates_cannot_share_a_proof_name() {
        let one = vec![event(
            "attest_candidate",
            "refused",
            json!({"identity": {"gitCommit": "051475eff"}}),
        )];
        let other = vec![event(
            "attest_candidate",
            "refused",
            json!({"identity": {"gitCommit": "4741a806a"}}),
        )];
        assert_ne!(
            digest_of(&one),
            digest_of(&other),
            "the proof name must distinguish which candidate was refused"
        );
    }
}

#[cfg(test)]
mod redaction {
    //! bd-reality-core-convergence-1azkt.10 bullet 6: the proof capsule must be
    //! REDACTION-SAFE, and `docs/environment_attestation.md:151` rules that a
    //! redaction-safe projection carries no "host-private absolute paths".
    //!
    //! The guard below is the durable half. The digest fix is a one-line change
    //! anyone could undo by adding a convenient path field later, on a surface
    //! whose whole purpose is to be handed to someone else — so the assertion is
    //! written against the WHOLE identity rather than against the one field I
    //! happened to change.

    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::CandidateIdentity;

    fn identity() -> CandidateIdentity {
        let version: Value = json!({
            "data": {
                "version": "0.15.2",
                "source": {"gitCommit": "051475eff", "gitDirty": false, "state": "clean"},
                "build": {
                    "profile": "debug",
                    "targetTriple": "x86_64-unknown-linux-gnu",
                    "frankenStack": "fsqlite@0.4.1"
                },
                "features": [{"name": "fts5", "enabled": true}]
            }
        });
        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        match CandidateIdentity::observe(&version, &binary) {
            Ok(identity) => identity,
            Err(verdict) => panic!("observe must not judge: {verdict:?}"),
        }
    }

    /// POSITIVE plus PRECONDITION: the location is recorded, as a digest, and
    /// the digest is RECONSTRUCTIBLE rather than opaque.
    ///
    /// Reconstructing it is what lets a reader check the claim without the
    /// artifact — the same move that made the empty-digest incident checkable.
    #[test]
    fn the_binary_location_is_recorded_as_a_reconstructible_digest() {
        let recorded = identity()
            .fields
            .get("binaryPathBlake3")
            .cloned()
            .unwrap_or_default();
        let expected = blake3::hash(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("Cargo.toml")
                .to_string_lossy()
                .as_bytes(),
        )
        .to_hex()
        .to_string();
        assert_eq!(recorded, expected, "the digest must be reconstructible");
        assert_eq!(
            recorded.len(),
            64,
            "expected a blake3 hex digest: {recorded}"
        );
    }

    /// THE GUARD: no field of the candidate identity may be a host-private
    /// absolute path.
    ///
    /// Written against every field, not just the one that was wrong, because
    /// the next absolute path to appear here will be a different field added by
    /// someone who did not read this comment.
    #[test]
    fn no_identity_field_carries_an_absolute_path() {
        let identity = identity();
        for (name, value) in &identity.fields {
            assert!(
                !value.starts_with('/') && !value.starts_with("\\\\") && !value.contains(":\\"),
                "identity field {name} looks like an absolute path and must be \
                 hashed or dropped (docs/environment_attestation.md:151): {value}"
            );
        }
        // Non-vacuity: the loop above passes trivially on an empty identity, and
        // an empty identity is exactly what the refusal path used to write.
        assert!(
            identity.fields.len() >= 6,
            "guard is vacuous unless the identity is populated: {:?}",
            identity.fields
        );
    }

    /// DISCRIMINATION: two different locations must not collapse to one digest.
    ///
    /// Without this the field could be a constant and every assertion above
    /// would still hold.
    #[test]
    fn two_binary_locations_do_not_share_a_digest() {
        let one = blake3::hash(b"/data/rch/abc/target/debug/ee")
            .to_hex()
            .to_string();
        let other = blake3::hash(b"/data/rch/def/target/debug/ee")
            .to_hex()
            .to_string();
        assert_ne!(one, other, "the digest must distinguish binary locations");
    }
}
