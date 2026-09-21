# Default canonical records

Captured from unmodified pre-fix `CanonicalDigest::of` at
`94783c0e8ebacfdc3c5ba91f600d51a3d3f63cd6`, default serde_json/BTreeMap graph,
remote run `20260906-170130-eeb5ddf8` (capture test process_exit=0).
The JSON freezes an actually stored Operation journal, its exact operation_json,
revision and transaction digests, model-grant scope and its digest, and a stored
collaboration grant. All data is synthetic; no user credentials or stores.

Do not regenerate after the digest fix. The ignored capture test is a historical
recipe only. Recovery imports the exact old journal bytes into the identical
Operation row and uses the production storage decoder after reopen. The same
fixed JSON is tested with default and serde_json/preserve_order dependencies.

This fixture proves compatibility with the previous normal default graph; it is
not an inventory of user stores written by experimental order-sensitive binaries.
