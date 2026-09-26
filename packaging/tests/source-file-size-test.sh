#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
MAX_LINES=2000
FAILED=0

while IFS= read -r file; do
    [[ -f "$REPO_ROOT/$file" ]] || continue
    lines="$(wc -l < "$REPO_ROOT/$file")"
    if ((lines > MAX_LINES)); then
        printf 'not ok - %s has %s lines; maximum is %s\n' "$file" "$lines" "$MAX_LINES" >&2
        FAILED=1
    fi
done < <(git -C "$REPO_ROOT" ls-files --cached --others --exclude-standard -- '*.rs' '*.sh' '*.py' '*.css' '*.yml' '*.yaml' '*.toml')

((FAILED == 0))
printf 'ok   - maintained code and script files stay at or below %s lines\n' "$MAX_LINES"
