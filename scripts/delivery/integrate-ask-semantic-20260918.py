#!/usr/bin/env python3
"""Bind the reviewed local semantic scorer to four source files; reject drift.

All replacements are validated before writes. Only the named Rust files are
changed; this script does not stage, commit, push, switch branches or delete.
The accompanying main-only delivery job handles formatting and publication.
"""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def replace(text, before, after, count=1):
    found = text.count(before)
    if found != count:
        raise SystemExit(f"Source drift: expected {count}, found {found}: {before[:120]!r}")
    return text.replace(before, after)


def transform_ask(text):
    text = replace(text,
        '//! abstention. Deterministic: same DB + question ⇒ byte-identical answer.',
        '//! abstention. The pure lexical evaluator is deterministic for fixed inputs.\n'
        '//! Local semantic evaluation additionally depends on the verified model.')
    text = replace(text, 'pub use native::AskNativeSource;\n', '''pub use native::AskNativeSource;

#[path = "ask_semantic.rs"]
mod semantic;

pub use semantic::evaluate_ask_with_local_model;

// One request-local scoring regime must drive admission and composition.
// A callback borrows the complete semantic table without adding mutable
// process-global state or changing the public request/candidate structures.
type SpanScorer<'a> = &'a dyn Fn(&[String], &str, f32, &str) -> f32;
''')
    text = replace(text, '''// Retained ADR §2 span-scoring weights; W1/W2 are documented design constants
// not yet consumed by the current scoring path.
#[allow(dead_code)]
const SPAN_W1_LEXICAL: f32 = 0.45;
#[allow(dead_code)]
const SPAN_W2_SEMANTIC: f32 = 0.35;''', '''// ADR §2 weights when a complete local semantic score table is available.
// The pure lexical path still redistributes W2 into W1.
const SPAN_W1_LEXICAL: f32 = 0.45;
const SPAN_W2_SEMANTIC: f32 = 0.35;''')
    text = replace(text, '''pub fn evaluate_ask(request: &AskRequest, candidates: &[AskCandidate]) -> AskReport {
    if !native::validate_sources(request, candidates) {
        return extractiveness_failure_report(request, candidates.len());
    }
    let mut report = evaluate_ask_inner(request, candidates);
    native::attach_sources(&mut report, request);
    report
}

fn evaluate_ask_inner(request: &AskRequest, candidates: &[AskCandidate]) -> AskReport {''', '''pub fn evaluate_ask(request: &AskRequest, candidates: &[AskCandidate]) -> AskReport {
    evaluate_ask_scored(request, candidates, &score_span, true)
}

fn request_is_valid(request: &AskRequest) -> bool {
    request.min_confidence.is_finite() && (0.0..=1.0).contains(&request.min_confidence)
}

fn evaluate_ask_scored(
    request: &AskRequest,
    candidates: &[AskCandidate],
    scorer: SpanScorer<'_>,
    semantic_degraded: bool,
) -> AskReport {
    if !request_is_valid(request) || !native::validate_sources(request, candidates) {
        return extractiveness_failure_report(request, candidates.len());
    }
    let mut report = evaluate_ask_inner(request, candidates, scorer, semantic_degraded);
    native::attach_sources(&mut report, request);
    report
}

fn evaluate_ask_inner(
    request: &AskRequest,
    candidates: &[AskCandidate],
    scorer: SpanScorer<'_>,
    semantic_degraded: bool,
) -> AskReport {''')
    start = text.index('fn evaluate_ask_inner(')
    end = text.index('\n/// Record an ask abstention', start)
    prefix, body, suffix = text[:start], text[start:end], text[end:]
    body = replace(body, 'selection::select_candidates(', 'selection::select_candidates_with_scorer(')
    body = replace(body, '        ASK_CANDIDATE_SCAN_CAP,\n', '        ASK_CANDIDATE_SCAN_CAP,\n        scorer,\n')
    body = replace(body, '            let score = score_span(', '            let score = scorer(')
    body = replace(body, 'semantic_degraded: true, // semantic always degraded in current impl', 'semantic_degraded,')
    body = replace(body, 'semantic_degraded: true,', 'semantic_degraded,', 2)
    return prefix + body + suffix


def transform_candidates(text):
    before_tests, separator, tests = text.partition('#[cfg(test)]')
    if not separator:
        raise SystemExit('Candidate module test boundary was not found')
    text = before_tests
    text = replace(text, 'AskCandidate, AskContradiction, AskRequest, has_negation,',
                   'AskCandidate, AskContradiction, AskRequest, SpanScorer, has_negation,')
    text = replace(text, 'fn best_span_score(question_terms: &[String], candidate: &AskCandidate) -> f32 {', '''fn best_span_score(
    question_terms: &[String],
    candidate: &AskCandidate,
    scorer: SpanScorer<'_>,
) -> f32 {''')
    text = replace(text, 'score_span(', 'scorer(', 3)
    for source in ['candidate', 'other', 'opposition.candidate']:
        text = replace(text, f'best_span_score(question_terms, {source})',
                       f'best_span_score(question_terms, {source}, scorer)')
    text = replace(text, "    ranked: &mut [RankedCandidate<'a>],\n",
                   "    ranked: &mut [RankedCandidate<'a>],\n    scorer: SpanScorer<'_>,\n", 2)
    for name in ['preserve_linked_opposition', 'preserve_inferred_opposition']:
        text = replace(text, f'{name}(request, question_terms, &unique, &mut ranked)',
                       f'{name}(request, question_terms, &unique, &mut ranked, scorer)')
    header = '''pub(super) fn select_candidates<'a>(
    request: &AskRequest,
    question_terms: &[String],
    candidates: &'a [AskCandidate],
    limit: usize,
) -> Result<Vec<&'a AskCandidate>, SelectionError> {'''
    text = replace(text, header, header + '''
    select_candidates_with_scorer(request, question_terms, candidates, limit, &score_span)
}

/// Use the same complete scorer for admission and the eventual answer.
pub(super) fn select_candidates_with_scorer<'a>(
    request: &AskRequest,
    question_terms: &[String],
    candidates: &'a [AskCandidate],
    limit: usize,
    scorer: SpanScorer<'_>,
) -> Result<Vec<&'a AskCandidate>, SelectionError> {''')
    return text + separator + tests


def transform_index(text):
    return replace(text, 'const EE_MODEL_CACHE_SUBDIR: &str = "models";\n', '''#[path = "index_ask.rs"]
mod ask_model;

pub(crate) use ask_model::local_ask_embedder;

const EE_MODEL_CACHE_SUBDIR: &str = "models";
''')


def transform_cli(text):
    marker = 'fn execute_ask(\n'
    if text.count(marker) != 1:
        raise SystemExit('Expected exactly one execute_ask handler')
    start = text.index(marker)
    # The function body ends before the next top-level function/item comment.
    # Exact unique anchors below avoid touching other command handlers.
    prefix, handler = text[:start], text[start:]
    handler = replace(handler,
        'DEGRADED_SEMANTIC, ask_data_json, evaluate_ask, ',
        'DEGRADED_SEMANTIC, ask_data_json, evaluate_ask_with_local_model, ')
    handler = replace(handler, '    let report = evaluate_ask(&request, &candidates);\n', '''    let report = match evaluate_ask_with_local_model(
        &connection,
        &workspace_id,
        &request,
        &candidates,
    ) {
        Ok(report) => report,
        Err(error) => return write_domain_error(&error, context, stdout, stderr),
    };
''')
    return prefix + handler


TRANSFORMS = {
    'src/core/ask.rs': transform_ask,
    'src/core/ask_candidates.rs': transform_candidates,
    'src/core/index.rs': transform_index,
    'src/cli/mod.rs': transform_cli,
}


def main():
    for name in TRANSFORMS:
        path = ROOT / name
        if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(ROOT):
            raise SystemExit(f'Not a regular source path inside the checkout: {name}')
    originals = {name: (ROOT / name).read_text() for name in TRANSFORMS}
    markers = {
        'src/core/ask.rs': 'pub use semantic::evaluate_ask_with_local_model;',
        'src/core/ask_candidates.rs': 'pub(super) fn select_candidates_with_scorer',
        'src/core/index.rs': 'pub(crate) use ask_model::local_ask_embedder;',
        'src/cli/mod.rs': 'let report = match evaluate_ask_with_local_model(',
    }
    integrated = [marker in originals[name] for name, marker in markers.items()]
    if any(integrated):
        if not all(integrated):
            raise SystemExit('Partial semantic integration detected; refusing ambiguous edits')
        print('All semantic bindings already present; no source rewrites needed')
        return
    if subprocess.check_output(['git', 'status', '--porcelain', '--', *TRANSFORMS], cwd=ROOT).strip():
        raise SystemExit('Affected files have uncommitted changes')
    changes = {name: transform(originals[name]) for name, transform in TRANSFORMS.items()}
    for name, before in originals.items():
        if (ROOT / name).read_text() != before:
            raise SystemExit(f'Source changed during preparation: {name}')
    for name, after in changes.items():
        (ROOT / name).write_text(after)
        print(f'Integrated {name}')


if __name__ == '__main__':
    main()
