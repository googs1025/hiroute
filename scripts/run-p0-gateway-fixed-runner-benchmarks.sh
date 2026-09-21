#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-p0-gateway-fixed-runner-benchmarks.sh \
  --base REVISION --candidate REVISION --output-dir PATH

Runs release-mode H1/H2, real hirouted Gateway-process, resource-path, and
disk-spill Replay benchmarks on one fixed runner. The base and candidate are checked out
as isolated worktrees. The candidate's benchmark harness is overlaid onto the
base worktree only, so product code remains at the requested exact base while
both revisions use identical measurement code.
USAGE
}

base=''
candidate=''
output_dir=''
while (($#)); do
  case "$1" in
    --base)
      base=${2:-}
      shift 2
      ;;
    --candidate)
      candidate=${2:-}
      shift 2
      ;;
    --output-dir)
      output_dir=${2:-}
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

if [[ ! "$base" =~ ^[0-9a-f]{40}$ || ! "$candidate" =~ ^[0-9a-f]{40}$ || -z "$output_dir" ]]; then
  usage >&2
  exit 2
fi
if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
  printf 'CARGO_TARGET_DIR must be unset; each worktree owns its default target directory\n' >&2
  exit 2
fi
if [[ $(uname -s) != Linux || ! -r /proc/self/status ]]; then
  printf 'fixed-runner production comparison requires a Linux runner with /proc\n' >&2
  exit 2
fi

repository=$(git rev-parse --show-toplevel)
harness_revision=$(git -C "$repository" rev-parse HEAD)
if [[ "$harness_revision" != "$candidate" ]]; then
  printf 'benchmark harness checkout (%s) must be the exact candidate revision (%s)\n' \
    "$harness_revision" "$candidate" >&2
  exit 2
fi
git -C "$repository" diff --quiet
git -C "$repository" diff --cached --quiet
test -z "$(git -C "$repository" status --porcelain)"
git -C "$repository" cat-file -e "${base}^{commit}"
git -C "$repository" cat-file -e "${candidate}^{commit}"

mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd)
work_root=$(mktemp -d "${TMPDIR:-/tmp}/hiroute-p0-fixed-runner.XXXXXX")
base_dir="$work_root/base"
candidate_dir="$work_root/candidate"

cleanup() {
  git -C "$repository" worktree remove --force "$base_dir" >/dev/null 2>&1 || true
  git -C "$repository" worktree remove --force "$candidate_dir" >/dev/null 2>&1 || true
  rm -rf "$work_root"
}
trap cleanup EXIT

git -C "$repository" worktree add --detach "$base_dir" "$base" >/dev/null
git -C "$repository" worktree add --detach "$candidate_dir" "$candidate" >/dev/null
git -C "$base_dir" diff --quiet
git -C "$candidate_dir" diff --quiet

# These are measurement-only files owned by this PROCESS. Overlaying them
# gives the exact base and candidate the same TTFT and Replay diagnostics;
# product source, Cargo.lock, and all runtime contracts stay at each revision.
mkdir -p "$base_dir/crates/gateway/benches/gateway_product_paths"
cp "$repository/crates/gateway-core/benches/gateway_core_baseline.rs" \
  "$base_dir/crates/gateway-core/benches/gateway_core_baseline.rs"
cp "$repository/crates/gateway/benches/gateway_replay_paths.rs" \
  "$base_dir/crates/gateway/benches/gateway_replay_paths.rs"
cp "$repository/crates/gateway/benches/gateway_product_paths.rs" \
  "$base_dir/crates/gateway/benches/gateway_product_paths.rs"
cp "$repository/crates/gateway/benches/gateway_product_paths/resource_metrics.rs" \
  "$base_dir/crates/gateway/benches/gateway_product_paths/resource_metrics.rs"
if ! grep -Eq '^name = "gateway_replay_paths"$' "$base_dir/crates/gateway/Cargo.toml"; then
  cat >> "$base_dir/crates/gateway/Cargo.toml" <<'TOML'

[[bench]]
name = "gateway_replay_paths"
harness = false
TOML
fi
if ! grep -Eq '^name = "gateway_product_paths"$' "$base_dir/crates/gateway/Cargo.toml"; then
  cat >> "$base_dir/crates/gateway/Cargo.toml" <<'TOML'

[[bench]]
name = "gateway_product_paths"
harness = false
required-features = ["e2e-test-control"]
TOML
fi

record_host() {
  {
    printf 'base_revision=%s\n' "$base"
    printf 'candidate_revision=%s\n' "$candidate"
    printf 'benchmark_harness_revision=%s\n' "$harness_revision"
    printf 'base_gateway_core_bench_sha256=%s\n' "$(sha256sum "$base_dir/crates/gateway-core/benches/gateway_core_baseline.rs" | awk '{print $1}')"
    printf 'candidate_gateway_core_bench_sha256=%s\n' "$(sha256sum "$candidate_dir/crates/gateway-core/benches/gateway_core_baseline.rs" | awk '{print $1}')"
    printf 'gateway_replay_bench_sha256=%s\n' "$(sha256sum "$repository/crates/gateway/benches/gateway_replay_paths.rs" | awk '{print $1}')"
    printf 'gateway_product_bench_sha256=%s\n' "$(sha256sum "$repository/crates/gateway/benches/gateway_product_paths.rs" | awk '{print $1}')"
    printf 'gateway_product_resource_metrics_sha256=%s\n' "$(sha256sum "$repository/crates/gateway/benches/gateway_product_paths/resource_metrics.rs" | awk '{print $1}')"
    uname -a
    rustc --version --verbose
    cargo --version --verbose
    cmake --version
    if command -v sccache >/dev/null 2>&1; then
      sccache --show-stats || true
    fi
  } > "$output_dir/host-metadata.txt"
}

run_revision() {
  local label=$1
  local directory=$2
  (
    cd "$directory"
    cargo build --release -p hiroute-gateway --bin hirouted --all-features \
      > "$output_dir/${label}-hirouted-build.log"
    HIROUTE_BENCH_ITERATIONS="${HIROUTE_BENCH_ITERATIONS:-100000}" \
      HIROUTE_ENFORCE_5_PERCENT=1 \
      cargo bench -p hiroute-gateway-core --bench gateway_core_baseline \
        --features pingora-transport > "$output_dir/${label}-gateway-core.tsv"
    HIROUTE_RESOURCE_BENCH_ITERATIONS="${HIROUTE_RESOURCE_BENCH_ITERATIONS:-10000}" \
      bash -ec 'for round in 1 2 3 4 5; do cargo bench -p hiroute-gateway-core --bench resource_paths; done' \
        > "$output_dir/${label}-resources.tsv"
    HIROUTE_GATEWAY_REPLAY_BENCH_ROUNDS="${HIROUTE_GATEWAY_REPLAY_BENCH_ROUNDS:-5}" \
      HIROUTE_GATEWAY_REPLAY_BENCH_BYTES="${HIROUTE_GATEWAY_REPLAY_BENCH_BYTES:-4194304}" \
      cargo bench -p hiroute-gateway --bench gateway_replay_paths \
        > "$output_dir/${label}-gateway-replay.tsv"
      HIROUTE_BENCH_HIROUTED_BIN="$PWD/target/release/hirouted" \
      HIROUTE_GATEWAY_PRODUCT_BENCH_ROUNDS="${HIROUTE_GATEWAY_PRODUCT_BENCH_ROUNDS:-5}" \
      HIROUTE_GATEWAY_PRODUCT_BENCH_ITERATIONS="${HIROUTE_GATEWAY_PRODUCT_BENCH_ITERATIONS:-5}" \
      HIROUTE_GATEWAY_PRODUCT_BENCH_BYTES="${HIROUTE_GATEWAY_PRODUCT_BENCH_BYTES:-716800}" \
      cargo bench -p hiroute-gateway --bench gateway_product_paths \
        --features e2e-test-control \
        > "$output_dir/${label}-gateway-product.tsv"
    # Cargo builds binary targets for integration-style benches and can
    # replace target/release/hirouted with the bench feature set before the
    # harness starts it. Capture the digest after that build so the evidence
    # is bound to the executable the product rows actually measured.
    sha256sum target/release/hirouted > "$output_dir/${label}-hirouted.sha256"
  )
}

record_host
run_revision base "$base_dir"
run_revision candidate "$candidate_dir"

python3 "$repository/scripts/summarize-p0-gateway-benchmarks.py" \
  --base-revision "$base" \
  --candidate-revision "$candidate" \
  --harness-revision "$harness_revision" \
  --base-core "$output_dir/base-gateway-core.tsv" \
  --candidate-core "$output_dir/candidate-gateway-core.tsv" \
  --base-resource "$output_dir/base-resources.tsv" \
  --candidate-resource "$output_dir/candidate-resources.tsv" \
  --base-replay "$output_dir/base-gateway-replay.tsv" \
  --candidate-replay "$output_dir/candidate-gateway-replay.tsv" \
  --base-product "$output_dir/base-gateway-product.tsv" \
  --candidate-product "$output_dir/candidate-gateway-product.tsv" \
  --base-hirouted-sha256 "$(awk '{print $1}' "$output_dir/base-hirouted.sha256")" \
  --candidate-hirouted-sha256 "$(awk '{print $1}' "$output_dir/candidate-hirouted.sha256")" \
  --output "$output_dir/summary.json"
