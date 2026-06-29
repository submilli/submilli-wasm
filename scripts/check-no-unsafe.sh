#!/usr/bin/env bash
# Enforces the zero-`unsafe` invariant (#34): no `unsafe {}` blocks or `unsafe fn`/`impl`/`trait`
# anywhere in `src`, except the two wasmtime-API-parity `unsafe fn`s — `Module::deserialize` /
# `deserialize_file` — which are signature-only (no unsafe operations) and marked
# `#[allow(unsafe_code)]`. This catches the one thing the `unsafe_code` lint can't: a real `unsafe {}`
# block smuggled in under an `#[allow(unsafe_code)]`. Test modules/fixtures (`*tests.rs`, `testdata/`)
# are excluded. Run locally with `./scripts/check-no-unsafe.sh`; CI runs the same check.
set -euo pipefail

cd "$(dirname "$0")/.."

# Match only real `unsafe` code constructs (`unsafe {`, `unsafe fn|impl|trait|extern`) — not the word
# in comments, the `unsafe_code` lint name, or `#[allow(unsafe_code)]`. Then drop test files and the
# allowlisted API-parity deserialize signatures.
hits=$(grep -rnE 'unsafe[[:space:]]+(fn|impl|trait|extern|\{)' src --include='*.rs' \
    | grep -vE '[^/]*tests\.rs:|/testdata/' \
    | grep -vE 'unsafe fn deserialize(_file)?\b' \
    || true)

if [ -n "$hits" ]; then
    echo "DISALLOWED unsafe found in src (the tree must stay zero-unsafe; #34):"
    echo "$hits"
    exit 1
fi

echo "no-unsafe check OK: zero unsafe ops in src (only API-parity unsafe fn signatures)"
exit 0
