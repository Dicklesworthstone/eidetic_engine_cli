#!/usr/bin/env bash
# bd-reality-core-convergence-1azkt.11 — execute both lexical relevance
# regression tests against the binary Gate 5 already built.
#
# WHY NOT `cargo test --test lexical_relevance_contract`
#
# Measured on an RCH fleet worker, base 82926d4c5: the two pins execute in 0.00s,
# but the cargo invocation around them took 74s — `Finished test profile in
# 1m 14s` with zero `Compiling` lines. That is cargo re-walking freshness across
# ~350 crates. Gate 5 (`cargo test --workspace --lib --bins --tests
# --examples`) has already paid for that exact scan moments earlier and has
# already produced this target's binary, so invoking cargo again charges the
# repository twice for one piece of work. Running the binary directly is not a
# way to make a budget fit; the double charge is waste in the gate whether or
# not anything is being added to it.
#
# Both arms are required. The guard exercises its listing check on synthetic
# input and verifies that the live binary is current and lists both tests.
# The run arm executes the tests and checks how many passed.
#
# Usage:
#   scripts/lexical_relevance_pins.sh --guard   required: prove the harness
#   scripts/lexical_relevance_pins.sh           required: run the pins

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PIN_SOURCE="${REPO_ROOT}/tests/lexical_relevance_contract.rs"
readonly PIN_ONE="lexical_score_kind_names_the_projection_that_produced_it"
readonly PIN_TWO="lexical_admission_does_not_invert_on_an_unrelated_documents_score"
readonly EXPECTED_PIN_COUNT=2

# Count the tests a libtest `--list` listing enumerates.
#
# Reads the `: test` suffix rather than the trailing `N tests, M benchmarks`
# summary, because a listing that enumerated nothing still prints a plausible
# summary line. Emits -1 when the input carries no listing at all, which is a
# failure in its own right rather than a zero.
pin_listed_test_count() {
    local listing="$1"
    if ! printf '%s\n' "$listing" | grep -q ': test$'; then
        printf '%s\n' "-1"
        return
    fi
    printf '%s\n' "$listing" | grep -c ': test$'
}

# Extract the passed-test count from a libtest `test result:` summary line.
# Emits -1 when no summary line is present so vacuous/malformed output fails.
pin_passed_test_count() {
    local run_output="$1"
    local summary_line
    summary_line="$(printf '%s\n' "$run_output" | grep -E '^test result: ' | tail -n 1 || true)"
    if [[ -z "$summary_line" ]]; then
        printf '%s\n' "-1"
        return
    fi
    if [[ "$summary_line" =~ ([0-9]+)[[:space:]]+passed ]]; then
        printf '%s\n' "${BASH_REMATCH[1]}"
    else
        printf '%s\n' "-1"
    fi
}

# Resolve the directory cargo writes build artifacts to.
#
# Env first, because that is what overrides everything else and is how RCH
# redirects builds to `.rch-target` on a Linux worker.
#
# This deliberately does NOT parse `.cargo/config.toml`. That file is gitignored
# and untracked by policy (AGENTS.md: "Don't commit the redirect to project
# config"), so it is a per-developer artifact this script cannot assume anything
# about — on the Mac dev host it carries `build.target-dir` pointing at an
# external USB volume. A run with neither variable set therefore resolves here
# to <repo>/target while cargo may resolve elsewhere, and the lookup below fails
# naming the directory it searched. That mismatch is INFORMATION and is reported
# rather than absorbed: see the note on pin_binary_path about why the search
# stays narrow.
pin_target_dir() {
    if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
        printf '%s\n' "$CARGO_TARGET_DIR"
    elif [[ -n "${CARGO_BUILD_TARGET_DIR:-}" ]]; then
        printf '%s\n' "$CARGO_BUILD_TARGET_DIR"
    else
        printf '%s\n' "${REPO_ROOT}/target"
    fi
}

# Locate the newest executable cargo built for the pin target.
#
# Prints nothing and returns 1 when there is no candidate, so the caller
# decides how loud that is. Never falls back to a stale search path: an absent
# binary must surface as absent, not as a zero-test run.
#
# THE GLOB STAYS NARROW ON PURPOSE. `debug/deps/` is where cargo puts an
# integration test binary, and if it is not there that is a fact about the
# target directory worth reporting, not an obstacle to route around. Widening
# this to a recursive search would convert a surprise into a silent green — the
# same move as loosening an assertion until it passes — and it is actively
# unsafe on a shared target dir: RCH has previously exited 0 while leaving
# wrong-platform artifacts behind, so a wide search could match a stale profile,
# another crate's binary, or a Linux ELF a Mac-side reader would then execute at
# ~0s and report success on. A narrow glob that fails loudly cannot do that.
pin_binary_path() {
    local target_dir
    target_dir="$(pin_target_dir)"
    local newest=""
    local candidate
    for candidate in \
        "${target_dir}"/debug/deps/lexical_relevance_contract-* \
        "${target_dir}"/debug/build/eidetic-engine/*/out/lexical_relevance_contract-* \
        "${target_dir}"/release/deps/lexical_relevance_contract-* \
        "${target_dir}"/release/build/eidetic-engine/*/out/lexical_relevance_contract-*; do
        [[ -f "$candidate" && -x "$candidate" ]] || continue
        case "$candidate" in
            *.d | *.o | *.rlib | *.rmeta | *.dSYM) continue ;;
        esac
        if [[ -z "$newest" || "$candidate" -nt "$newest" ]]; then
            newest="$candidate"
        fi
    done
    [[ -n "$newest" ]] || return 1
    printf '%s\n' "$newest"
}

if [[ "${1:-}" == "--guard" ]]; then
    failures=0

    check() {
        local label="$1" input="$2" expected="$3"
        local actual
        actual="$(pin_listed_test_count "$input")"
        if [[ "$actual" == "$expected" ]]; then
            echo "ok   - $label"
        else
            echo "FAIL - $label: got '$actual', wanted '$expected'"
            failures=$((failures + 1))
        fi
    }

    # Prove the predicate can FAIL before trusting it to pass. An empty
    # listing and a listing whose summary claims tests it never enumerated are
    # the two shapes that would otherwise read as a healthy harness.
    check "empty listing is -1, not 0" "" "-1"
    check "summary without entries is -1, not 2" \
        "2 tests, 0 benchmarks" "-1"
    check "a real two-test listing counts 2" \
        "${PIN_ONE}: test
${PIN_TWO}: test

2 tests, 0 benchmarks" \
        "2"
    check "a listing that lost one pin counts 1" \
        "${PIN_ONE}: test

1 test, 0 benchmarks" \
        "1"

    check_passed() {
        local label="$1" input="$2" expected="$3"
        local actual
        actual="$(pin_passed_test_count "$input")"
        if [[ "$actual" == "$expected" ]]; then
            echo "ok   - $label"
        else
            echo "FAIL - $label: got '$actual', wanted '$expected'"
            failures=$((failures + 1))
        fi
    }

    check_passed "empty test run output is -1, not 2" "" "-1"
    check_passed "0 passed test result counts 0 (fails >= ${EXPECTED_PIN_COUNT} check)" \
        "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s" \
        "0"
    check_passed "2 passed test result counts 2" \
        "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s" \
        "2"

    if [[ $failures -ne 0 ]]; then
        echo "guard predicate is broken; ${failures} self-test(s) failed" >&2
        exit 1
    fi

    # The predicate works. Apply it to the live artifact.
    if [[ ! -f "$PIN_SOURCE" ]]; then
        echo "pin source missing: ${PIN_SOURCE}" >&2
        exit 1
    fi

    binary="$(pin_binary_path)" || {
        echo "no lexical_relevance_contract binary under $(pin_target_dir)/debug/deps." >&2
        echo "Gate 5 (cargo test --workspace --tests) builds it; this stage must run after it." >&2
        exit 1
    }
    echo "ok   - binary present: ${binary}"

    if [[ "$PIN_SOURCE" -nt "$binary" ]]; then
        echo "FAIL - ${PIN_SOURCE} is newer than ${binary}; the pins on disk are not the pins that would run" >&2
        exit 1
    fi
    echo "ok   - binary is not older than its source"

    listing="$("$binary" --list 2>&1)"
    listed="$(pin_listed_test_count "$listing")"
    if [[ "$listed" != "$EXPECTED_PIN_COUNT" ]]; then
        echo "FAIL - expected ${EXPECTED_PIN_COUNT} pins, binary enumerates ${listed}" >&2
        printf '%s\n' "$listing" >&2
        exit 1
    fi
    echo "ok   - binary enumerates ${EXPECTED_PIN_COUNT} pins"

    for pin in "$PIN_ONE" "$PIN_TWO"; do
        if ! printf '%s\n' "$listing" | grep -q "^${pin}: test$"; then
            echo "FAIL - pin ${pin} is not in the binary's listing" >&2
            printf '%s\n' "$listing" >&2
            exit 1
        fi
        echo "ok   - pin present: ${pin}"
    done

    echo "lexical relevance pin harness verified"
    exit 0
fi

binary="$(pin_binary_path)" || {
    cat >&2 <<BANNER
Lexical relevance tests could not start. No executable matched
lexical_relevance_contract-* in the Cargo test artifact directories under
$(pin_target_dir). Run the test build and point CARGO_TARGET_DIR at its output.
BANNER
    exit 1
}

run_output="$("$binary" --test-threads=1 2>&1)"
run_status=$?
if [[ -n "$run_output" ]]; then
    printf '%s\n' "$run_output"
fi
if [[ $run_status -ne 0 ]]; then
    exit $run_status
fi

passed_count="$(pin_passed_test_count "$run_output")"
if [[ "$passed_count" -lt "$EXPECTED_PIN_COUNT" ]]; then
    echo "FAIL - expected at least ${EXPECTED_PIN_COUNT} tests executed and passed, got '${passed_count}'" >&2
    exit 1
fi
exit 0
