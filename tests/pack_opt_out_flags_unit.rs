#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use ee::core::context::{
    ContextPackOutputOptionOverrides, ContextPackOutputOptions, ContextPackOutputProfile,
};
use ee::models::{MemoryId, ProvenanceUri, TrustClass, UnitScore};
use ee::output::{ContextJsonRenderOptions, render_context_response_json_with_options};
use ee::pack::{
    ContextPackProfile, ContextRequest, ContextResponse, PackAssemblyOptions, PackCandidate,
    PackCandidateInput, PackProvenance, PackSection, PackTrustSignal, TokenBudget,
    assemble_draft_with_profile_and_options,
};
use serde_json::Value;
use uuid::Uuid;

type TestResult = Result<(), String>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MatrixCase {
    name: &'static str,
    profile: ContextPackOutputProfile,
    overrides: ContextPackOutputOptionOverrides,
}

fn memory_id(seed: u128) -> MemoryId {
    MemoryId::from_uuid(Uuid::from_u128(seed))
}

fn unit(value: f32) -> UnitScore {
    UnitScore::parse(value).expect("unit score accepts fixture value")
}

fn provenance(seed: u32) -> PackProvenance {
    PackProvenance::new(
        ProvenanceUri::from_str(&format!("file://tests/pack_opt_out/{seed}.md"))
            .expect("fixture provenance URI parses"),
        "pack opt-out fixture",
    )
    .expect("fixture provenance constructs")
}

fn candidate(seed: u128, content: &str, relevance: f32, utility: f32) -> PackCandidate {
    let provenance_seed = u32::try_from(seed).expect("fixture seed fits u32");
    PackCandidate::new(PackCandidateInput {
        memory_id: memory_id(seed),
        section: PackSection::ProceduralRules,
        content: content.to_owned(),
        estimated_tokens: 8,
        relevance: unit(relevance),
        utility: unit(utility),
        provenance: vec![provenance(provenance_seed)],
        why: format!("fixture candidate {seed} matches release work"),
    })
    .expect("fixture candidate constructs")
    .with_trust_signal(PackTrustSignal::new(
        TrustClass::HumanExplicit,
        Some("fixture".to_owned()),
    ))
}

fn response_for(options: ContextPackOutputOptions) -> ContextResponse {
    let query = "prepare release opt-out profile";
    let request = ContextRequest::from_query(query).expect("fixture request accepts");
    let budget = TokenBudget::new(400).expect("fixture budget accepts");
    let candidates = vec![
        candidate(1, "Run cargo fmt --check before release.", 0.95, 0.80),
        candidate(
            2,
            "Run cargo clippy --all-targets before tagging.",
            0.90,
            0.75,
        ),
        candidate(3, "Run cargo fmt --check before release.", 0.85, 0.70),
    ];
    let mut draft = assemble_draft_with_profile_and_options(
        ContextPackProfile::Balanced,
        query,
        budget,
        candidates,
        PackAssemblyOptions {
            include_coverage_fill: options.include_coverage_fill,
        },
    )
    .expect("fixture draft assembles");
    draft.hash = Some(format!("blake3:fixture-{}", options.profile.as_str()));
    ContextResponse::new(request, draft, Vec::new()).expect("fixture response constructs")
}

fn render_value(
    response: &ContextResponse,
    options: ContextPackOutputOptions,
) -> Result<Value, String> {
    let rendered = render_context_response_json_with_options(
        response,
        ContextJsonRenderOptions::from(options),
    );
    serde_json::from_str(&rendered).map_err(|error| format!("JSON did not parse: {error}"))
}

fn assert_case(case: MatrixCase) -> TestResult {
    let options =
        ContextPackOutputOptions::for_profile(case.profile).with_overrides(case.overrides);
    let response = response_for(options);
    let coverage_fill_count = response.data.pack.coverage_fill_count() as u64;
    let value = render_value(&response, options)?;
    let pack = value
        .pointer("/data/pack")
        .ok_or_else(|| format!("{} missing data.pack", case.name))?;

    ensure_presence(
        pack.get("text").is_some(),
        options.include_rendered_text,
        case.name,
        "pack.text",
    )?;
    ensure_presence(
        pack.get("skipped").is_some(),
        options.include_skipped,
        case.name,
        "pack.skipped",
    )?;
    ensure_presence(
        pack.get("meta").is_some(),
        options.include_meta,
        case.name,
        "pack.meta",
    )?;

    if options.include_coverage_fill && coverage_fill_count == 0 {
        return Err(format!("{} expected coverage fill entries", case.name));
    }
    if !options.include_coverage_fill && coverage_fill_count != 0 {
        return Err(format!(
            "{} expected coverage fill disabled, got {coverage_fill_count}",
            case.name
        ));
    }

    ensure_presence(
        pack.pointer("/meta/selectionFormula").is_some(),
        options.include_meta && options.include_verbose_meta,
        case.name,
        "pack.meta.selectionFormula",
    )
}

fn ensure_presence(actual: bool, expected: bool, case_name: &str, field: &str) -> TestResult {
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "{case_name}: expected {field} presence={expected}, got {actual}"
        ))
    }
}

#[test]
fn pack_opt_out_matrix_controls_json_shape_and_coverage_fill() -> TestResult {
    let mut cases = Vec::new();
    for no_coverage_fill in [None, Some(true)] {
        for no_rendered_text in [None, Some(true)] {
            for no_skipped in [None, Some(true)] {
                for no_meta in [None, Some(true)] {
                    cases.push(MatrixCase {
                        name: "standard_binary_matrix",
                        profile: ContextPackOutputProfile::Standard,
                        overrides: ContextPackOutputOptionOverrides {
                            no_coverage_fill,
                            no_rendered_text,
                            no_skipped,
                            no_meta,
                        },
                    });
                }
            }
        }
    }

    cases.extend([
        MatrixCase {
            name: "profile_lean",
            profile: ContextPackOutputProfile::Lean,
            overrides: ContextPackOutputOptionOverrides::default(),
        },
        MatrixCase {
            name: "profile_standard",
            profile: ContextPackOutputProfile::Standard,
            overrides: ContextPackOutputOptionOverrides::default(),
        },
        MatrixCase {
            name: "profile_verbose",
            profile: ContextPackOutputProfile::Verbose,
            overrides: ContextPackOutputOptionOverrides::default(),
        },
        MatrixCase {
            name: "lean_override_coverage_fill_back_on",
            profile: ContextPackOutputProfile::Lean,
            overrides: ContextPackOutputOptionOverrides {
                no_coverage_fill: Some(false),
                ..ContextPackOutputOptionOverrides::default()
            },
        },
        MatrixCase {
            name: "lean_override_rendered_text_back_on",
            profile: ContextPackOutputProfile::Lean,
            overrides: ContextPackOutputOptionOverrides {
                no_rendered_text: Some(false),
                ..ContextPackOutputOptionOverrides::default()
            },
        },
        MatrixCase {
            name: "lean_override_skipped_back_on",
            profile: ContextPackOutputProfile::Lean,
            overrides: ContextPackOutputOptionOverrides {
                no_skipped: Some(false),
                ..ContextPackOutputOptionOverrides::default()
            },
        },
        MatrixCase {
            name: "lean_override_meta_off",
            profile: ContextPackOutputProfile::Lean,
            overrides: ContextPackOutputOptionOverrides {
                no_meta: Some(true),
                ..ContextPackOutputOptionOverrides::default()
            },
        },
        MatrixCase {
            name: "verbose_suppresses_rendered_text",
            profile: ContextPackOutputProfile::Verbose,
            overrides: ContextPackOutputOptionOverrides {
                no_rendered_text: Some(true),
                ..ContextPackOutputOptionOverrides::default()
            },
        },
    ]);

    if cases.len() != 24 {
        return Err(format!("expected 24 A8 matrix cases, got {}", cases.len()));
    }
    for case in cases {
        assert_case(case)?;
    }
    Ok(())
}
