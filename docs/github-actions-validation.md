# GitHub Actions validation

GitHub Actions runs on the checked-out commit through `scripts/ci-run.py`.

## Workflows

| File | Execution | Scope |
| --- | --- | --- |
| `gateway-core.yml` | PR, main push, manual, reusable | Affected PR scope or full main Linux backend formatting, size boundary, clippy, tests, E2E contract; separate frontend build/tests and model-data generator checks |
| `p0-gateway-gates.yml` | Manual, reusable | Linux listener build, neutral loopback, H1/H2 lifecycle, replay/privacy tests, contract checks, benchmark harness smoke |
| `gateway-core-dedicated.yml` | Manual or reusable | Hosted Linux production stability and socket churn |
| `p0-gateway-final-gates.yml` | Manual | Aggregate the three current hosted workflows |

Rust jobs select `Default Larger Runners`: `ubuntu-latest-8-cores` for normal
checks and `ubuntu-latest-16-cores` for stability. The organization must permit
the repository to use this group. Frontend and result-only jobs use the standard
GitHub-hosted Ubuntu runner. Jobs receive read-only repository permissions and
checkout does not persist credentials. No repository or deployment secrets are provided to
pull-request jobs.

The toolchain comes from `rust-toolchain.toml`. Cargo sources are cached, not
`target/`. Build concurrency defaults to the runner CPU count; incremental
compilation is disabled. Every job keeps its own checkout-local Cargo output.
The hosted job is disposable, so there is no persistent worktree cleanup service.

## Evidence and failures

Each invocation checks the full expected SHA against HEAD and rejects tracked
source changes. On pull requests this is GitHub's checked-out test commit, not
an inferred branch tip. The wrapper waits synchronously and returns failure
on process failure, timeout, revision mismatch, or a required Rust test invocation
with no passing tests. It preserves the child process exit separately from its
own decision. `scenario_state: not_assessed` is intentional: process success does
not establish product scenario acceptance.

`artifacts/ci/<name>/` contains the complete command log and JSON result, including
revision, CPU count, platform, timestamps, and Cargo/Rust versions for Cargo
commands. Failure prints the log tail; Actions uploads available artifacts even
when a check fails. Timeout and cancellation stop the command process group
before removing its private short TMPDIR. A forcibly terminated VM cannot be
expected to finish uploading artifacts.

The adapter can be tested without Rust compilation or remote access:

```sh
python3 scripts/test-ci-run.py
```

## Coverage boundaries

The fixed-machine benchmark and semantic result-checking scripts remain available for
maintainer validation; hosted benchmark smoke is not fixed-machine performance evidence.

The hosted Rust checks exclude `hiroute-desktop`. Frontend build/tests do not
prove native Tauri interaction. macOS Desktop remains on the existing local
validation path; these workflows provide no Windows or ARM validation evidence.
Real-provider credentials and separately provisioned production E2E fixtures are
not supplied by this CI setup. Existing failing checks remain failures, not
expected-red success or waived gates.

Tooling-only pull requests run their selected Python checks in the selection job and retain
CI reports. Contributors can run `python3 scripts/test-plan.py --base origin/main` to inspect
the affected command set before opening a pull request.
