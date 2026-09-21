# MVP-18 local Worker launcher

Implements the MVP-20 consumer `WorkerPlatformPort`. It owns OS child objects,
stdio, and explicitly requested temporary materials. It does not interpret ACP,
permissions, Harness configuration, Plan, tokens, or sessions.

## Branch convergence

The consumer slice is from exactly
`7dddd8a1149de53645bcc538728a74e72fd05064`:

- `delegation/platform.rs` is imported unchanged.
- `delegation/profile/mod.rs` imports the existing profile builder without its
  unrelated tests. No profile behavior is changed.
- `delegation/acp/mod.rs` contains only the existing identity-contract enum,
  without the ACP transport/driver or unused imports.
- `domain/delegation/mod.rs` imports the permit/intent/error types and existing
  authorization methods required to compile that builder, plus the existing
  `DelegationProcessBindingV1`; unrelated progress/records/tests are excluded.
- `domain/delegation/process.rs` is unchanged.

These are branch-local consumer dependencies, not a second platform API. The
coordinator replaces their exports with the final consumer tree at convergence.
Shared intent: export `delegation::local_worker`, enable Tokio `process`, add the
existing workspace Rustix dependency to daemon on Unix, use the already locked
`same-file=1.0.6` safe directory handle for root identity on both OSes, and converge Cargo.lock.

The additional fixed materials slice is exactly
`95ee7f4b1c55c77d9e38bcb78a662d1306dbf2ce`. Its profile builder/materials module
and required `valid_delegation_id` helper are imported unchanged (excluding
unrelated profile tests). TaskSessionRoot preparation/history inventory remain
20's code and responsibility; local_worker only consumes their output.

`launch` directly consumes `CandidateWorkerProfile.materials` and `session_root`.
There is no preparation registry, path template, or alternative launch API.
The external session root must already exist and must not equal or nest with the
new private root. Materials may contain only directories and an empty file list.
The launcher never derives ownership from HOME/CODEX_HOME/CLAUDE_CONFIG_DIR.
The launcher checks the explicit executable path and basic executability without
reading program contents or verifying pins. Installation discovery and Harness
profile rendering remain with the selection path.

On Unix each child starts a fresh process group. Safe `waitid(NOWAIT)` observes
without reaping the group leader; the retained leader protects group identity
through signalling, including root-first exit. After reaping, group identifiers
are used only for read-only absence checks, never another destructive signal.
A stopped group does not prove no deliberately detached process exists. Windows
uses a held child only: root scope remains insufficient proof of ordinary child
chain cancellation until native Windows validation and any required mechanism.

Tests execute this production module with a no-model OS probe. They do not prove
real Harness/ACP or CLI/Desktop wiring; those remain consumer/integration checks.

Cleanup keeps an open directory identity handle, verifies the current root is the
same object, and rejects symlink/reparse replacement before deleting its own root.
This avoids authorizing deletion solely from a stale path/inode number.
