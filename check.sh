#!/usr/bin/env bash
# Runs the checks a change to this repository should pass. Every check runs,
# even after one fails, and the end says which failed.
#
#   ./check.sh                  everything
#   ./check.sh rust web         some of it: rust, debugger-core, web
#
# The rust part needs cargo-deny (cargo install cargo-deny) and wasm-pack.
# The TypeScript packages need Node, and install their dependencies the first
# time. CARGO_BUILD_JOBS limits cargo's parallel jobs where memory is short.

set -uo pipefail
cd "$(dirname "$0")"

parts=("$@")
if [ ${#parts[@]} -eq 0 ]; then
    parts=(rust debugger-core web)
fi

failed=()

# run <label> <command...>: runs the command and records a failure.
run() {
    local label="$1"
    shift
    echo "-- $label"
    if ! "$@"; then
        failed+=("$label")
    fi
}

# npm_in <folder> <script...>: an npm script of one of the TypeScript packages.
npm_in() {
    local folder="$1"
    shift
    (cd "$folder" && { [ -d node_modules ] || npm ci; } && npm run "$@")
}

for part in "${parts[@]}"; do
    case "$part" in
        rust)
            run "cargo fmt" cargo fmt --all --check
            run "cargo clippy" cargo clippy --workspace --all-targets -- -D warnings
            run "cargo test" cargo test --workspace
            # Each benchmark runs once, as a test: one that panics before it
            # measures anything fails here instead of going unseen.
            run "benchmarks, one pass each" cargo test --workspace --bench '*'
            # The WebAssembly crates' tests only run in a WebAssembly build.
            run "wasm tests (sg1)" wasm-pack test --node cores/sg1/wasm
            run "wasm tests (snes1)" wasm-pack test --node cores/snes1/wasm
            run "licenses" cargo deny check licenses
            ;;
        debugger-core)
            run "debugger-core tests and coverage" npm_in debugger-core test:coverage
            run "debugger-core types" npm_in debugger-core typecheck
            ;;
        web)
            run "web tests" npm_in web test
            run "web types" npm_in web typecheck
            run "web build" npm_in web build
            ;;
        *)
            echo "unknown part: $part (the parts are rust, debugger-core and web)" >&2
            exit 2
            ;;
    esac
done

echo
if [ ${#failed[@]} -gt 0 ]; then
    echo "failed: ${failed[*]}"
    exit 1
fi
echo "every check passed"
