#!/usr/bin/env bash
set -euo pipefail

soft_limit=700
hard_limit=1100
checked_files=0
failed=0

while IFS= read -r -d '' rust_file; do
  [[ -f "${rust_file}" ]] || continue
  line_count=$(awk 'END { print NR }' "${rust_file}")
  checked_files=$((checked_files + 1))

  if (( line_count > hard_limit )); then
    printf 'error: %s has %d lines (hard limit: %d)\n' \
      "${rust_file}" "${line_count}" "${hard_limit}" >&2
    failed=1
  elif (( line_count > soft_limit )); then
    printf 'warning: %s has %d lines (soft target: %d)\n' \
      "${rust_file}" "${line_count}" "${soft_limit}" >&2
  fi
done < <(git ls-files -z --cached --others --exclude-standard -- '*.rs')

printf 'checked %d Rust files (soft target: %d, hard limit: %d)\n' \
  "${checked_files}" "${soft_limit}" "${hard_limit}"
exit "${failed}"
