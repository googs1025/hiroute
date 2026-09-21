# HiRoute CLI

HiRoute CLI is the official terminal entry to the HiRoute local service. It can connect to Desktop or serve as the complete management interface for Linux headless. Model sources, smart routes, Agent connections, session observation, and Worker tasks reuse the same production paths as Desktop; there is no second headless control plane.

## Install and inspect the CLI

- macOS Desktop: open Settings → CLI → Terminal entry and select Install.
- Linux headless: follow [Run HiRoute headless on Linux](/en/docs/install-linux/) to use the website's one-line installer, then start the service explicitly.

If the entry directory is not on PATH, add this line to your shell configuration and open a new terminal:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Inspect the entry and released commands:

```sh
hiroute --help
hiroute schema list --output json
hiroute schema show --command-id worker.exec --output json
```

The CLI entry and local-service availability are separate states. If a command reports that the service is unavailable, Desktop users should open the app. Standalone users should run `hiroute service status`, then `hiroute service start` when needed. The CLI never starts the service or replays a failed request automatically.

## Two command contracts

Discover Host management commands through root and family help:

```sh
hiroute --help
hiroute service --help
hiroute gateway --help
hiroute protected-input --help
```

Discover Application / Local Control commands through the Released schema and complete leaf help:

```sh
hiroute schema list --output json
hiroute schema show --command-id routing.apply --output json
hiroute routing apply --help
```

Do not expect `schema list` to contain Host commands, and do not infer fields for the installed version from a website snippet.

## Manage models, routes, and Agents

The Released CLI now supports the complete headless loop:

- Discover and inspect model sources with `compute scan/list/show`; check and save connections with `compute connection options/test/preview/apply/authorize`.
- Inspect candidate model capabilities with `models show`.
- Create, update, and publish smart routes with `routing options/list/show/preview/apply`.
- Discover local Agents with `agents scan/list/check`; connect with `agents connect preview/apply/status` and recover with `agents restore preview/apply`.
- If a write response is lost or uncertain, query the operation in its original idempotency domain with `operations find/get`.

Every write moves from `preview` to `apply` with the same change, digest, revisions, and idempotency key. Pass passwords and API keys through `protected-input`, never through ordinary JSON, arguments, or logs. Obtain exact fields from the matching `schema show`, `options`, and leaf `--help`.

## Inspect sessions and runtime performance

```sh
hiroute sessions list --include-unlinked --limit 50 --output json
hiroute sessions show <SESSION_ID> --output json
hiroute sessions receipt <RECEIPT_ID> --output json
hiroute sessions status --output json
hiroute value show --routing <PLAN_ID> --session <SESSION_ID> --output json
```

Session queries return facts and a timeline by default, not conversation bodies. A receipt reports the actual route, model, and upstream-reported tokens. When no trustworthy price evidence exists, monetary value remains unknown instead of being fabricated as zero.

## Discover executors and plans

```sh
hiroute worker executors
hiroute worker plans
```

The results contain the execution agents available on this device and the published plans allowed for the current agent. Do not infer a plan ID from its display name; use the ID returned by the command.

## Start a task

```sh
hiroute worker exec \
  --plan <PLAN_ID> \
  --cwd /absolute/path/to/project \
  --title "Investigate failing tests" \
  --submission-key my-check-001 \
  -- "Find the cause, propose the smallest fix, and run the relevant checks"
```

Supply exactly one input source: text after `--`, a `--file`, or standard input. `--cwd` is converted to an absolute path, but it is not directory authorization or a concurrency lock.

`--submission-key` is a caller-selected idempotency key. If a connection or process interruption leaves acceptance uncertain, keep the same key and query it. Do not generate a new key and retry blindly:

```sh
hiroute worker status --submission my-check-001 --operation start
```

## Read progress and results

Use the run ID returned at admission:

```sh
hiroute worker status --run <RUN_ID>
hiroute worker wait --run <RUN_ID>
hiroute worker result --run <RUN_ID>
```

`wait` is bounded and never cancels a task that is still running. `result` supports offset and maximum-byte options for paging through a large result.

## Continue or cancel

Continuation requires both the task ID and its exact latest run ID:

```sh
hiroute worker continue \
  --task <TASK_ID> \
  --expected-latest-run <RUN_ID> \
  --submission-key my-check-002 \
  -- "Finish the fix based on the test result"
```

Cancel one exact run:

```sh
hiroute worker cancel --run <RUN_ID> --reason user-requested
```

Cancellation does not undo files or external effects already produced.

## Machine-readable output

Public commands support `--output text|json|quiet`. Use the default `text` interactively, `json` for scripts and main agents that consume the schema, and `quiet` when only the exit result matters. Use `hiroute schema list` and `hiroute schema show` to discover the current machine contract at runtime.

The CLI currently publishes 45 Released Application commands. CLI and daemon continue to reject all remaining Planned commands. Automation should discover the runtime schema instead of hard-coding the command count or unreleased capabilities.
