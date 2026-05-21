//! Serialise a [`Lockfile`] to deterministic JSON.

use super::schema::{LockEntry, Lockfile};

/// Render a lockfile to JSON.
///
/// Determinism guarantees:
///  - Top-level keys: `lockfileVersion`, `manifestHash`, `entries` (in that
///    order, fixed by the `Lockfile` struct's field order).
///  - `entries` map keys: alphabetical, courtesy of `BTreeMap`.
///  - Each entry's `agents` and `dependencies` arrays: alphabetical.
///  - Trailing newline (POSIX-friendly diffs).
pub fn write_lockfile(lockfile: &Lockfile) -> String {
    // Clone so we can sort the per-entry arrays without mutating the caller's
    // value. Cost is fine for v0.1; lockfiles are small.
    let mut sorted = lockfile.clone();
    for entry in sorted.entries.values_mut() {
        sort_entry_arrays(entry);
    }

    let mut out = serde_json::to_string_pretty(&sorted)
        .expect("Lockfile with String keys serializes infallibly");
    out.push('\n');
    out
}

fn sort_entry_arrays(entry: &mut LockEntry) {
    entry.agents.sort_unstable();
    entry.dependencies.sort_unstable();
}
