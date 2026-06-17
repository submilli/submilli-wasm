#!/usr/bin/env bash
# Enforces the STYLE.md file-size cap: no src/**/*.rs over 400 lines (300 is the target).
# Run locally with `./scripts/check-line-count.sh`; CI runs the same check.
set -euo pipefail

cd "$(dirname "$0")/.."

MAX=400
status=0

while IFS= read -r -d '' file; do
    lines=$(wc -l <"$file" | tr -d '[:space:]')
    if [ "$lines" -gt "$MAX" ]; then
        echo "TOO LONG ($lines > $MAX): $file"
        status=1
    fi
done < <(find src -name '*.rs' -print0)

if [ "$status" -eq 0 ]; then
    echo "file-size check OK: all src/**/*.rs <= $MAX lines"
fi
exit "$status"
