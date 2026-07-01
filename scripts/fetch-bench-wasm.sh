#!/usr/bin/env bash
# Fetches the large .wasm fixtures for the lifecycle benchmarks (benches/).
# They're gitignored (5.6 MB total) to keep the repo lean; the tiny
# coremark-minimal.wasm is committed. Run this once before `cargo bench
# --bench lifecycle` or `cargo run --example bench_table`. CI runs it too.
#
# Idempotent: skips files already present unless `--force`.
set -euo pipefail

cd "$(dirname "$0")/.."
dest="benches/wasm"
mkdir -p "$dest"

wasmi="https://raw.githubusercontent.com/wasmi-labs/wasmi/main/crates/wasmi/benches/wasm"

# name  url
fixtures=(
    "spidermonkey.wasm    $wasmi/spidermonkey.wasm"
    "pulldown-cmark.wasm  $wasmi/pulldown-cmark.wasm"
)

force=0
[ "${1:-}" = "--force" ] && force=1

for entry in "${fixtures[@]}"; do
    read -r name url <<<"$entry"
    out="$dest/$name"
    if [ "$force" -eq 0 ] && [ -s "$out" ]; then
        echo "have    $name"
        continue
    fi
    echo "fetch   $name"
    curl -fSL --retry 3 -o "$out" "$url"
done

echo "OK: benchmark fixtures present in $dest"
