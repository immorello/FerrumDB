# Contributing

Changes to FerrumDB should be tested against the failure mode they can realistically affect. The project has two regression tracks: **correctness** and **speed**. Include the commands you ran, the machine, and the storage type in your PR or commit notes.

Do not send PRs that touch the WAL, recovery, or locking path without proving that durability invariants still hold. The only acceptable speed regression is when it is the direct cost of fixing a correctness bug — and that trade-off must be stated explicitly.

---

## Correctness Regression Tests

Run the full test suite:

```
cargo test
```

All 31 tests must pass before opening a PR. The test suites and what they cover:

- **`tests/wal.rs`** — append, read-back, persistence across instances, clear. Any change to the WAL file format or encoding must pass this.
- **`tests/recovery.rs`** — WAL replay on restart, delete replay, checkpoint, snapshot + WAL combined recovery, sequence continuity. Changes to `open_with_paths` or the replay loop must pass this.
- **`tests/store.rs`** — sorted iteration, set/get/delete correctness, sorted order after WAL replay and checkpoint. Changes to `Store` internals must pass this.
- **`tests/lock.rs`** — double-open rejection, lock release on drop, per-table isolation, multi-cycle reacquisition. Any change to the locking path must pass this.
- **`tests/transaction.rs`** — commit visibility, rollback on drop, crash recovery of committed transactions, uncommitted entry discard. Changes to the COMMIT path or `Transaction` must pass this.
- **`tests/perf.rs`** — perf tests run as correctness checks too; they will fail if the code panics or produces wrong results.

---

## Durability Invariants

These must never be broken:

1. **WAL before BTreeMap** — the WAL entry must be written and flushed before in-memory state is updated. A crash between WAL write and BTreeMap update must be safe.
2. **COMMIT before apply** — `Transaction::commit` must write all WAL entries and then `write_commit` (which fsyncs) before modifying the BTreeMap. On recovery, entries without a following COMMIT must be silently discarded.
3. **One writer** — the exclusive `flock` must be held for the full lifetime of a `Store`. No code path may bypass or early-release it.

If you believe a change is safe to make without one of these guarantees, explain why in the PR with a concrete crash scenario.

---

## Speed Regression Tests

Run the perf suite with output visible:

```
cargo test perf -- --nocapture
```

When comparing two commits, run on the same machine, the same storage device, and the same background load. Report at minimum:

- `[write]` single write throughput before and after
- `[batch tx]` batched transaction throughput before and after
- Machine type and storage (e.g. "Raspberry Pi 4, eMMC" or "MacBook M3, APFS")

Note that macOS APFS has unusually high fsync latency (~10–15ms). Linux on the actual embedded target (~1–5ms) will show materially different numbers. If you only have a Mac, say so.

---

## Dependency Discipline

FerrumDB intentionally has one runtime dependency (`prost`). Before adding a crate, ask:

- Can the standard library do this?
- Can a direct syscall do this (`extern "C"`)?
- Is this crate's full transitive dependency tree acceptable on an embedded Linux target?

The file locking using `flock` via `extern "C"` instead of the `fs4` crate is a deliberate example of this discipline. Follow it.

---

## Commit Hygiene

- One logical change per commit. A WAL format change and a refactor of `Store` are two commits.
- The commit message should say *why*, not just *what*. The diff already says what.
- If the change fixes a correctness bug, describe the failure scenario in the commit body.
- Do not commit generated files, IDE folders, or anything that belongs in `.gitignore`.
