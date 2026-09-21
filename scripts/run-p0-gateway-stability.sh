#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-p0-gateway-stability.sh --scenario sixty-second|ten-minute --result PATH

Builds the release hirouted product binary and a production-subprocess driver
before timing begins. The result records the driver-process exit separately
from semantic RSS and replay-retention evidence; neither signal can make the
other green.
USAGE
}

scenario=''
result=''
while (($#)); do
  case "$1" in
    --scenario)
      scenario=${2:-}
      shift 2
      ;;
    --result)
      result=${2:-}
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$scenario" || -z "$result" ]]; then
  usage >&2
  exit 2
fi
if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
  printf "CARGO_TARGET_DIR must be unset; the dedicated run uses this worktree's default target directory\n" >&2
  exit 2
fi
if [[ $(uname -s) != Linux || ! -r /proc/self/status ]]; then
  printf 'dedicated production RSS stability requires a Linux runner with /proc\n' >&2
  exit 2
fi

case "$scenario" in
  sixty-second|ten-minute)
    ;;
  *)
    printf 'unknown stability scenario: %s\n' "$scenario" >&2
    exit 2
    ;;
esac

result_dir=$(dirname "$result")
mkdir -p "$result_dir"
log="${result%.json}.log"
revision=$(git rev-parse HEAD)

# Compilation is intentionally outside the driver process evidence. The
# executable that receives the measurement is the built release binary below.
cargo build --locked --release -p hiroute-gateway --bin hirouted --all-features \
  >"${result%.json}.hirouted-build.log" 2>&1
cargo build --locked --release -p hiroute-gateway --bench gateway_product_paths --all-features \
  --message-format=json >"${result%.json}.driver-build.json" 2>&1

hirouted="$PWD/target/release/hirouted"
if [[ ! -x "$hirouted" ]]; then
  printf 'release hirouted binary was not built at %s\n' "$hirouted" >&2
  exit 1
fi
driver=$(python3 - "${result%.json}.driver-build.json" <<'PY'
import json
import pathlib
import sys

for line in pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace").splitlines():
    try:
        message = json.loads(line)
    except json.JSONDecodeError:
        continue
    target = message.get("target", {})
    if target.get("name") == "gateway_product_paths" and message.get("executable"):
        print(message["executable"])
        break
else:
    raise SystemExit("release gateway_product_paths driver executable was not reported by Cargo")
PY
)
hirouted_sha256=$(sha256sum "$hirouted" | awk '{print $1}')

set +e
HIROUTE_STABILITY_HIROUTED_BIN="$hirouted" \
  "$driver" --stability "$scenario" >"$log" 2>&1
driver_process_exit=$?
set -e

set +e
python3 ./scripts/check-p0-gateway-stability-result.py \
  --driver-log "$log" \
  --scenario "$scenario" \
  --revision "$revision" \
  --hirouted-sha256 "$hirouted_sha256" \
  --driver-process-exit "$driver_process_exit" \
  --result "$result"
semantic_exit=$?
set -e

if [[ "$driver_process_exit" -ne 0 ]]; then
  exit "$driver_process_exit"
fi
exit "$semantic_exit"
