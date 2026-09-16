#!/usr/bin/env bash
# bd-up1hk — run the mcp module's LIB unit tests, and refuse a vacuous pass.
#
# src/mcp.rs carries 59 `#[test]` functions behind `#[cfg(feature = "mcp")]`
# (src/lib.rs:38-39). `mcp` is not a default feature, so a default build does
# not compile the module at all. The only mcp-enabled CI job runs
# `--test mcp_parity`, an INTEGRATION target. Net effect before this script:
# `cargo test --features mcp --lib` ran nowhere and all 59 unit tests were
# compiled and executed by no gate.
#
# The trap this script exists to avoid is subtle and is why the naive fix is
# worse than nothing: `cargo test --lib mcp::` WITHOUT `--features mcp`
# compiles a crate that has no `mcp` module, matches zero tests, prints
# "0 passed", and EXITS 0. Wiring that into CI would create a second false
# green in the exact shape this bead was filed to remove. So the executed-test
# count is parsed and a zero count is a hard failure.
#
# Usage:
#   scripts/mcp_lib_tests.sh              run the real cargo arm
#   scripts/mcp_lib_tests.sh --self-test  prove the vacuity guard, no cargo

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Extract the number of tests cargo reported as executed.
#
# Reads the `test result:` summary rather than the `running N tests` header:
# a filtered run prints `running 0 tests` and then a result line, and the
# result line is what carries the authoritative passed/failed counts.
# Emits -1 when no result line is present at all, which is itself a failure
# (cargo did not get far enough to report).
mcp_executed_test_count() {
    local output="$1"
    local count
    count=$(printf '%s\n' "$output" \
        | sed -n 's/^test result:.*[^0-9]\([0-9][0-9]*\) passed.*/\1/p' \
        | tail -1)
    if [[ -z "$count" ]]; then
        printf '%s\n' "-1"
    else
        printf '%s\n' "$count"
    fi
}

if [[ "${1:-}" == "--self-test" ]]; then
    failures=0
    check() {
        local label="$1" input="$2" expected="$3"
        local actual
        actual="$(mcp_executed_test_count "$input")"
        if [[ "$actual" == "$expected" ]]; then
            echo "ok   - $label"
        else
            echo "FAIL - $label: got '$actual', wanted '$expected'"
            failures=$((failures + 1))
        fi
    }

    # The exact shape of the false green this bead exists to prevent: the
    # feature is absent, the filter matches nothing, cargo exits 0.
    check "filtered-to-nothing run reports zero" \
        "running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9728 filtered out; finished in 0.00s" \
        "0"

    # A real run.
    check "real run reports its executed count" \
        "running 59 tests

test result: ok. 59 passed; 0 failed; 0 ignored; 0 measured; 9669 filtered out; finished in 1.21s" \
        "59"

    # A failing run still reports how many executed.
    check "failing run still reports passed count" \
        "test result: FAILED. 57 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.30s" \
        "57"

    # Cargo never reached a summary (compile error, abort).
    check "no result line is distinguishable from zero" \
        "error[E0433]: failed to resolve: use of undeclared crate or module \`mcp\`" \
        "-1"

    # Multiple summaries (doc-tests etc.): the last one wins deterministically.
    check "last result line wins" \
        "test result: ok. 59 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.2s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.1s" \
        "3"

    echo "self-test: $((5 - failures))/5 passed"
    [[ "$failures" -eq 0 ]] || exit 2
    exit 0
fi

cd "$REPO_ROOT" || exit 3

# --features mcp is load-bearing: without it the filter matches nothing and
# the command succeeds having run no tests.
OUTPUT=$(cargo test --workspace --features mcp --lib --jobs 1 mcp:: \
    -- --test-threads=1 2>&1)
CARGO_RC=$?
printf '%s\n' "$OUTPUT"

EXECUTED="$(mcp_executed_test_count "$OUTPUT")"

if [[ "$EXECUTED" == "-1" ]]; then
    echo "mcp_lib_tests: cargo produced no 'test result:' summary; treating as failure" >&2
    exit 1
fi

if [[ "$EXECUTED" -eq 0 ]]; then
    echo "mcp_lib_tests: REFUSING a vacuous pass — cargo executed 0 tests." >&2
    echo "mcp_lib_tests: the mcp:: filter matched nothing, which usually means" >&2
    echo "mcp_lib_tests: --features mcp was dropped and src/mcp.rs was not compiled." >&2
    exit 1
fi

echo "mcp_lib_tests: executed ${EXECUTED} mcp lib test(s)" >&2
exit "$CARGO_RC"
