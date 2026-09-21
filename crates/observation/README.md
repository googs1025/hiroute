# Local observation

`hiroute-observation` stores and queries execution facts and managed conversation content. It does not make routing decisions or use ordinary logs as a source of execution truth.

## Responsibilities

- `writer`, `receipt`, and `store`: bounded fact ingestion and durable local storage.
- `query` and `query_v2`: request, session, task, and turn views with explicit unknown or incomplete associations.
- `content`, `managed_text`, and `text_index`: content references, bounded reads, search, and deletion boundaries.
- `valuation` and `value`: usage and monetary facts with explicit missing-price and incomplete-data states.
- `maintenance`: storage maintenance and retention work.

Observation gaps must remain visible without changing the model request's execution result. Historical identity and price facts must not be inferred from current configuration. Deleted content remains unavailable through search, old references, and replay.

Managed content implementation details are described in
[managed_text/README.md](src/managed_text/README.md). The executable storage, query,
deletion and retention expectations are covered by this crate's tests and the product E2E
scenarios under `e2e/product`.

Focused package checks use `cargo test -p hiroute-observation`.
