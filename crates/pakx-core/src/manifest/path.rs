//! Dot-path access into the raw YAML tree backing `agents.yml`.
//!
//! Companion to [`crate::manifest::mutate`]: where that module rewrites
//! the **typed** [`crate::manifest::Manifest`] (with full schema
//! validation), this module operates on `serde_yaml_ng::Value` so the
//! `pakx manifest get/set/delete` surface can address arbitrary fields
//! — including ones the typed schema doesn't model (e.g. unknown keys,
//! forward-compat fields, the per-version `sponsors` list once a
//! manifest grows one). `pakx manifest set` is a pure-text mutator by
//! design: schema validation happens at `pakx pack` / `pakx test`
//! time, not here.
//!
//! Path syntax mirrors `npm pkg get/set/delete`:
//!   - `description` — top-level key
//!   - `dependencies.skills` — nested key
//!   - `dependencies.skills[0]` — first array element
//!   - `dependencies.mcp[1].agents` — keys + indices interleave freely
//!
//! Caveats locked in until v1:
//!   - YAML round-tripping does NOT preserve comments. The `serde_yaml_ng`
//!     loader drops them at parse time, so `pakx manifest set` will
//!     strip any comments the source carried. The `manifest` subcommand
//!     surfaces this in its help text.
//!   - Negative indices are not supported (`npm pkg` rejects them too).

use serde_yaml_ng::Value;

/// One step in a parsed path. Either a YAML mapping key or a sequence
/// index. Indices may only appear after a sequence-valued parent;
/// validation happens at `apply` time, not at parse time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    /// `foo`, `bar` — a YAML mapping key.
    Key(String),
    /// `[N]` — a YAML sequence index. Stored as `usize` so the
    /// callers (get/set/delete) never have to range-check at use site;
    /// negative indices are rejected in [`parse_path`].
    Index(usize),
}

/// Failure cases for [`parse_path`] and the apply helpers. Each variant
/// carries a short, user-facing message so the CLI can render the same
/// diagnostic across get/set/delete without reformatting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// Empty input — `pakx manifest get ""` and friends.
    #[error("manifest path must not be empty")]
    Empty,
    /// Malformed bracket syntax: missing closer, non-numeric body, or
    /// the bracket opening before a key segment (e.g. `[0].name` is
    /// allowed and means "index into the top-level sequence", but the
    /// **top-level** is always a mapping in `agents.yml` so this is
    /// still rejected at the apply layer rather than the parser).
    #[error("invalid path segment near `{0}`")]
    BadSegment(String),
    /// Caller asked to descend through a scalar (`description.foo`).
    /// The set/delete paths can't intuit what to overwrite, so we bail
    /// rather than silently clobbering.
    #[error("cannot descend into scalar at `{0}`")]
    DescendScalar(String),
    /// Index out of bounds for the sequence at this point in the path.
    /// Surfaced for get/delete; set may extend a sequence by exactly
    /// one (push-on-end) and only raises this for any further gap.
    #[error("index {index} out of bounds (length {len}) at `{at}`")]
    IndexOutOfBounds {
        index: usize,
        len: usize,
        at: String,
    },
    /// A key segment was applied to a sequence (`skills.0` instead of
    /// `skills[0]`). Surfaced explicitly so the user knows to use
    /// bracket syntax for indexing rather than guessing.
    #[error("expected sequence index `[N]` but got key `{key}` at `{at}`")]
    KeyOnSequence { key: String, at: String },
    /// An index segment was applied to a mapping. Mirror of
    /// `KeyOnSequence` for the opposite mismatch.
    #[error("expected mapping key but got index `[{index}]` at `{at}`")]
    IndexOnMapping { index: usize, at: String },
}

/// Outcome of [`delete_value`]. The CLI uses this to differentiate
/// "removed something" (silent success) from "nothing to remove"
/// (idempotent warning on stderr; exit 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// The path resolved to a real entry that was removed.
    Removed,
    /// The path didn't resolve to anything. Caller chooses how to
    /// surface the no-op.
    NotPresent,
}

/// Parse a dot-path with optional `[N]` indices into a segment list.
///
/// Returns an error for empty input, malformed brackets, or negative
/// indices. Does NOT consult the manifest tree — semantic validation
/// (e.g. "this key exists") happens in [`get_value`] / [`set_value`] /
/// [`delete_value`].
pub fn parse_path(raw: &str) -> Result<Vec<PathSeg>, PathError> {
    if raw.is_empty() {
        return Err(PathError::Empty);
    }
    let mut segments: Vec<PathSeg> = Vec::new();
    let mut buf = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                // A `.` either finishes a key segment or separates an
                // index segment from the next key (`mcp[1].agents`).
                // Empty buf is only legal immediately after a `]`
                // segment — guard against double-dot / leading-dot.
                if buf.is_empty() {
                    if !matches!(segments.last(), Some(PathSeg::Index(_))) {
                        return Err(PathError::BadSegment(raw.to_owned()));
                    }
                    continue;
                }
                segments.push(PathSeg::Key(std::mem::take(&mut buf)));
            }
            '[' => {
                // Close out any pending key first.
                if !buf.is_empty() {
                    segments.push(PathSeg::Key(std::mem::take(&mut buf)));
                }
                // Collect digits until `]`.
                let mut num = String::new();
                let mut closed = false;
                for d in chars.by_ref() {
                    if d == ']' {
                        closed = true;
                        break;
                    }
                    num.push(d);
                }
                if !closed || num.is_empty() {
                    return Err(PathError::BadSegment(raw.to_owned()));
                }
                let idx: usize = num
                    .parse()
                    .map_err(|_| PathError::BadSegment(raw.to_owned()))?;
                segments.push(PathSeg::Index(idx));
                // After `]` we expect either end-of-input, a `.`, or
                // another `[`. Anything else (e.g. `[0]foo`) is
                // malformed.
                if let Some(&peek) = chars.peek() {
                    if peek != '.' && peek != '[' {
                        return Err(PathError::BadSegment(raw.to_owned()));
                    }
                }
            }
            ']' => {
                // A bare `]` without a matching `[`.
                return Err(PathError::BadSegment(raw.to_owned()));
            }
            other => buf.push(other),
        }
    }
    if !buf.is_empty() {
        segments.push(PathSeg::Key(buf));
    }
    if segments.is_empty() {
        return Err(PathError::Empty);
    }
    Ok(segments)
}

/// Resolve `path` against `root`.
///
/// Returns `None` if any segment doesn't exist — the no-error
/// "missing" case the CLI maps to exit 1 (or `null` under `--json`).
/// Real malformed-path errors surface from [`parse_path`] before we
/// ever get here.
#[must_use]
pub fn get_value<'a>(root: &'a Value, path: &[PathSeg]) -> Option<&'a Value> {
    let mut cur = root;
    for seg in path {
        match seg {
            PathSeg::Key(k) => {
                cur = cur.as_mapping()?.get(Value::String(k.clone()))?;
            }
            PathSeg::Index(i) => {
                cur = cur.as_sequence()?.get(*i)?;
            }
        }
    }
    Some(cur)
}

/// Resolve `path` against a `serde_json::Value` tree.
///
/// Mirror of [`get_value`] for JSON shapes — the `pakx info <id>
/// <field>` field-query path walks the registry's JSON response, not
/// `agents.yml`. Path syntax and semantics are identical (segments
/// reuse [`PathSeg`] and [`parse_path`]); the only difference is the
/// underlying value type.
///
/// Returns `None` for any missing segment (out-of-bounds index, absent
/// key, or descending through a scalar) — the caller (CLI field-query
/// surface) maps that to exit 1 + a `null` stdout under `--json`.
#[must_use]
pub fn get_value_json<'a>(
    root: &'a serde_json::Value,
    path: &[PathSeg],
) -> Option<&'a serde_json::Value> {
    let mut cur = root;
    for seg in path {
        match seg {
            PathSeg::Key(k) => {
                cur = cur.as_object()?.get(k)?;
            }
            PathSeg::Index(i) => {
                cur = cur.as_array()?.get(*i)?;
            }
        }
    }
    Some(cur)
}

/// Write `value` at `path` inside `root`. Creates intermediate
/// mappings as needed; refuses to fabricate intermediate sequences
/// (the user must point at an existing sequence with `[N]` syntax).
///
/// Setting an index `N` where `N == len` pushes onto the end of an
/// existing sequence. Setting `N > len` is rejected with
/// [`PathError::IndexOutOfBounds`] — `npm pkg set` behaves the same
/// way and the alternative (auto-padding with nulls) breeds invalid
/// manifests.
pub fn set_value(root: &mut Value, path: &[PathSeg], value: Value) -> Result<(), PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    set_inner(root, path, value, &mut String::new())
}

fn set_inner(
    cur: &mut Value,
    path: &[PathSeg],
    value: Value,
    breadcrumb: &mut String,
) -> Result<(), PathError> {
    let (head, tail) = path.split_first().expect("non-empty checked by caller");
    let is_last = tail.is_empty();
    match head {
        PathSeg::Key(k) => {
            push_crumb(breadcrumb, head);
            // Replace a `Null` parent with a fresh mapping so newly
            // initialised manifests (or freshly-set deep paths) don't
            // require manual scaffolding. Refusing the conversion
            // would force the user to set every intermediate key
            // individually, which is exactly the friction `pakx
            // manifest set` is meant to remove.
            if cur.is_null() {
                *cur = Value::Mapping(serde_yaml_ng::Mapping::new());
            }
            // Snapshot the kind before taking the `&mut Mapping` so
            // the error-builder closure can read it without overlapping
            // borrows.
            let is_sequence = cur.is_sequence();
            let Some(map) = cur.as_mapping_mut() else {
                return Err(mapping_kind_mismatch_err(is_sequence, head, breadcrumb));
            };
            if is_last {
                map.insert(Value::String(k.clone()), value);
                return Ok(());
            }
            // Recurse — create the right empty intermediate based on
            // the **next** segment so deep paths work on a fresh
            // manifest. If the next segment is an `Index`, scaffold
            // an empty sequence; otherwise (the next segment is a
            // `Key`) scaffold an empty mapping. Always scaffolding a
            // mapping would force a follow-up `Index` recursion to
            // bottom out in `IndexOnMapping`, breaking ergonomic deep
            // sets like `foo[0]` on a fresh manifest.
            let key = Value::String(k.clone());
            if !map.contains_key(&key) {
                map.insert(key.clone(), empty_for_next(tail));
            }
            let next = map.get_mut(&key).expect("just inserted");
            set_inner(next, tail, value, breadcrumb)
        }
        PathSeg::Index(i) => {
            push_crumb(breadcrumb, head);
            let is_mapping = cur.is_mapping();
            let Some(seq) = cur.as_sequence_mut() else {
                return Err(sequence_kind_mismatch_err(is_mapping, head, breadcrumb));
            };
            let len = seq.len();
            if *i > len {
                return Err(PathError::IndexOutOfBounds {
                    index: *i,
                    len,
                    at: breadcrumb.clone(),
                });
            }
            if is_last {
                if *i == len {
                    seq.push(value);
                } else {
                    seq[*i] = value;
                }
                return Ok(());
            }
            if *i == len {
                // Auto-extend with the shape the **next** segment
                // demands. Same reasoning as the mapping branch above
                // — picking `Mapping` unconditionally breaks
                // `foo[0][0]` style paths because the inner `[0]`
                // would land on a mapping and fail with
                // `IndexOnMapping`. Pick the empty container by
                // peeking at the next segment.
                seq.push(empty_for_next(tail));
            }
            set_inner(&mut seq[*i], tail, value, breadcrumb)
        }
    }
}

/// Pick the right empty intermediate container to scaffold when a
/// `set` path passes through a missing segment.
///
/// `tail` is the remaining path past the current segment. The first
/// element of `tail` tells us what shape the next recursion expects:
/// a `PathSeg::Index` recursion needs a `Value::Sequence`, a
/// `PathSeg::Key` recursion needs a `Value::Mapping`. Empty `tail`
/// (i.e. the current segment is the last one) never reaches this
/// helper — the leaf branches insert the user-supplied value
/// directly.
fn empty_for_next(tail: &[PathSeg]) -> Value {
    match tail.first() {
        Some(PathSeg::Index(_)) => Value::Sequence(serde_yaml_ng::Sequence::new()),
        _ => Value::Mapping(serde_yaml_ng::Mapping::new()),
    }
}

/// Remove the entry at `path`. Returns [`DeleteOutcome::NotPresent`]
/// when any segment along the way doesn't exist (idempotent — the
/// caller treats it as a warning, not an error).
pub fn delete_value(root: &mut Value, path: &[PathSeg]) -> Result<DeleteOutcome, PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    delete_inner(root, path, &mut String::new())
}

fn delete_inner(
    cur: &mut Value,
    path: &[PathSeg],
    breadcrumb: &mut String,
) -> Result<DeleteOutcome, PathError> {
    let (head, tail) = path.split_first().expect("non-empty checked by caller");
    let is_last = tail.is_empty();
    match head {
        PathSeg::Key(k) => {
            push_crumb(breadcrumb, head);
            // Snapshot before borrowing mutably.
            let is_null = cur.is_null();
            let is_sequence = cur.is_sequence();
            let Some(map) = cur.as_mapping_mut() else {
                // Refusing-to-descend on a scalar is a real error.
                // Missing-mapping-on-Null is a soft no-op (the value
                // simply doesn't exist).
                if is_null {
                    return Ok(DeleteOutcome::NotPresent);
                }
                return Err(mapping_kind_mismatch_err(is_sequence, head, breadcrumb));
            };
            let key = Value::String(k.clone());
            if is_last {
                return Ok(if map.remove(&key).is_some() {
                    DeleteOutcome::Removed
                } else {
                    DeleteOutcome::NotPresent
                });
            }
            let Some(next) = map.get_mut(&key) else {
                return Ok(DeleteOutcome::NotPresent);
            };
            delete_inner(next, tail, breadcrumb)
        }
        PathSeg::Index(i) => {
            push_crumb(breadcrumb, head);
            let is_null = cur.is_null();
            let is_mapping = cur.is_mapping();
            let Some(seq) = cur.as_sequence_mut() else {
                if is_null {
                    return Ok(DeleteOutcome::NotPresent);
                }
                return Err(sequence_kind_mismatch_err(is_mapping, head, breadcrumb));
            };
            if *i >= seq.len() {
                return Ok(DeleteOutcome::NotPresent);
            }
            if is_last {
                seq.remove(*i);
                return Ok(DeleteOutcome::Removed);
            }
            delete_inner(&mut seq[*i], tail, breadcrumb)
        }
    }
}

fn push_crumb(breadcrumb: &mut String, seg: &PathSeg) {
    match seg {
        PathSeg::Key(k) => {
            if !breadcrumb.is_empty() {
                breadcrumb.push('.');
            }
            breadcrumb.push_str(k);
        }
        PathSeg::Index(i) => {
            use std::fmt::Write;
            let _ = write!(breadcrumb, "[{i}]");
        }
    }
}

/// Build the "expected a mapping but found something else" error.
///
/// `parent_is_sequence` should be true when the parent value is a
/// YAML sequence — that promotes the generic [`PathError::DescendScalar`]
/// to the more precise [`PathError::KeyOnSequence`] so the user knows
/// to switch to bracket-index syntax. The breadcrumb already includes
/// the offending segment thanks to `push_crumb` in the caller, so we
/// trim it back to the parent for the error message (so the user reads
/// "at `dependencies`" not "at `dependencies.skills`").
fn mapping_kind_mismatch_err(
    parent_is_sequence: bool,
    seg: &PathSeg,
    breadcrumb: &str,
) -> PathError {
    let at = trim_one_segment(breadcrumb, seg);
    if parent_is_sequence {
        if let PathSeg::Key(k) = seg {
            return PathError::KeyOnSequence { key: k.clone(), at };
        }
    }
    PathError::DescendScalar(at)
}

/// Mirror of [`mapping_kind_mismatch_err`] for the index-on-non-sequence
/// case. `parent_is_mapping` true → upgrade to
/// [`PathError::IndexOnMapping`].
fn sequence_kind_mismatch_err(
    parent_is_mapping: bool,
    seg: &PathSeg,
    breadcrumb: &str,
) -> PathError {
    let at = trim_one_segment(breadcrumb, seg);
    if parent_is_mapping {
        if let PathSeg::Index(i) = seg {
            return PathError::IndexOnMapping { index: *i, at };
        }
    }
    PathError::DescendScalar(at)
}

/// Strip the trailing `.<seg>` or `[<seg>]` so error messages point at
/// the parent path rather than the offending leaf.
fn trim_one_segment(breadcrumb: &str, seg: &PathSeg) -> String {
    match seg {
        PathSeg::Key(k) => {
            let with_dot = format!(".{k}");
            breadcrumb.strip_suffix(&with_dot).map_or_else(
                || breadcrumb.trim_start_matches(k).to_owned(),
                str::to_owned,
            )
        }
        PathSeg::Index(i) => {
            let bracketed = format!("[{i}]");
            breadcrumb
                .strip_suffix(&bracketed)
                .map_or_else(|| breadcrumb.to_owned(), str::to_owned)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml_ng::Value;

    fn sample() -> Value {
        serde_yaml_ng::from_str(
            "name: demo\nversion: 0.1.0\ndescription: a demo\ndependencies:\n  skills:\n    - alice/bob@0.1.0\n    - carol/dave\n  mcp:\n    - registry: official\n      name: filesystem\n",
        )
        .unwrap()
    }

    #[test]
    fn parse_path_handles_keys_and_indices() {
        assert_eq!(
            parse_path("name").unwrap(),
            vec![PathSeg::Key("name".into())]
        );
        assert_eq!(
            parse_path("dependencies.skills[0]").unwrap(),
            vec![
                PathSeg::Key("dependencies".into()),
                PathSeg::Key("skills".into()),
                PathSeg::Index(0),
            ]
        );
        assert_eq!(
            parse_path("dependencies.mcp[1].agents").unwrap(),
            vec![
                PathSeg::Key("dependencies".into()),
                PathSeg::Key("mcp".into()),
                PathSeg::Index(1),
                PathSeg::Key("agents".into()),
            ]
        );
        assert_eq!(parse_path("[0]").unwrap(), vec![PathSeg::Index(0)]);
    }

    #[test]
    fn parse_path_rejects_malformed_input() {
        assert!(matches!(parse_path("").unwrap_err(), PathError::Empty));
        assert!(matches!(
            parse_path(".foo").unwrap_err(),
            PathError::BadSegment(_)
        ));
        assert!(matches!(
            parse_path("a..b").unwrap_err(),
            PathError::BadSegment(_)
        ));
        assert!(matches!(
            parse_path("a[").unwrap_err(),
            PathError::BadSegment(_)
        ));
        assert!(matches!(
            parse_path("a[]").unwrap_err(),
            PathError::BadSegment(_)
        ));
        assert!(matches!(
            parse_path("a[abc]").unwrap_err(),
            PathError::BadSegment(_)
        ));
        assert!(matches!(
            parse_path("a]b").unwrap_err(),
            PathError::BadSegment(_)
        ));
        assert!(matches!(
            parse_path("a[0]b").unwrap_err(),
            PathError::BadSegment(_)
        ));
    }

    #[test]
    fn get_value_resolves_keys_indices_and_returns_none_on_miss() {
        let root = sample();
        let path = parse_path("description").unwrap();
        assert_eq!(get_value(&root, &path).unwrap().as_str(), Some("a demo"));

        let path = parse_path("dependencies.skills[0]").unwrap();
        assert_eq!(
            get_value(&root, &path).unwrap().as_str(),
            Some("alice/bob@0.1.0")
        );

        let path = parse_path("dependencies.skills[99]").unwrap();
        assert!(get_value(&root, &path).is_none());

        let path = parse_path("nope").unwrap();
        assert!(get_value(&root, &path).is_none());
    }

    #[test]
    fn set_value_overwrites_existing_scalar() {
        let mut root = sample();
        let path = parse_path("description").unwrap();
        set_value(&mut root, &path, Value::String("new desc".into())).unwrap();
        assert_eq!(get_value(&root, &path).unwrap().as_str(), Some("new desc"));
    }

    #[test]
    fn set_value_pushes_when_index_equals_len() {
        let mut root = sample();
        let path = parse_path("dependencies.skills[2]").unwrap();
        set_value(&mut root, &path, Value::String("eve/frank@0.2.0".into())).unwrap();
        assert_eq!(
            get_value(&root, &path).unwrap().as_str(),
            Some("eve/frank@0.2.0")
        );
        // Earlier entries untouched.
        let p0 = parse_path("dependencies.skills[0]").unwrap();
        assert_eq!(
            get_value(&root, &p0).unwrap().as_str(),
            Some("alice/bob@0.1.0")
        );
    }

    #[test]
    fn set_value_rejects_gap_past_len() {
        let mut root = sample();
        let path = parse_path("dependencies.skills[5]").unwrap();
        let err = set_value(&mut root, &path, Value::String("x".into())).unwrap_err();
        assert!(matches!(err, PathError::IndexOutOfBounds { .. }));
    }

    #[test]
    fn set_value_creates_intermediate_mappings_on_fresh_root() {
        let mut root: Value = serde_yaml_ng::from_str("name: demo\nversion: 0.1.0\n").unwrap();
        let path = parse_path("metadata.repo.url").unwrap();
        set_value(
            &mut root,
            &path,
            Value::String("https://example.test".into()),
        )
        .unwrap();
        assert_eq!(
            get_value(&root, &path).unwrap().as_str(),
            Some("https://example.test")
        );
    }

    /// Round-47 regression: `set_value` used to scaffold every missing
    /// intermediate as an empty mapping. When the **next** path
    /// segment was an `Index` (e.g. `foo[0]` where `foo` is missing,
    /// or `foo[0][0]` where the auto-pushed sequence element is also
    /// missing), the recursion bottomed out in
    /// `PathError::IndexOnMapping` and the user got a confusing error
    /// for a path that ought to "just work" on a fresh manifest.
    ///
    /// The fix: peek the next segment when scaffolding and pick a
    /// `Value::Sequence` instead of a `Value::Mapping` whenever the
    /// next recursion will expect an index.
    #[test]
    fn set_value_scaffolds_sequences_when_next_segment_is_index() {
        // Fresh empty manifest — every intermediate is missing.
        let mut root: Value = serde_yaml_ng::from_str("name: demo\n").unwrap();
        let path = parse_path("foo[0][0]").unwrap();
        set_value(&mut root, &path, Value::String("bar".into())).unwrap();

        // Expected shape:  foo: [[bar]]
        let foo = root
            .as_mapping()
            .unwrap()
            .get(Value::String("foo".into()))
            .unwrap();
        let outer = foo.as_sequence().expect("foo must be a sequence");
        assert_eq!(outer.len(), 1);
        let inner = outer[0]
            .as_sequence()
            .expect("foo[0] must be a sequence (auto-extended on Index recursion)");
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].as_str(), Some("bar"));
    }

    /// Sibling pin: `foo[0].name = bar` on a fresh manifest must
    /// still scaffold `foo` as a sequence and `foo[0]` as a mapping
    /// (because the next-after-the-index segment is a `Key`). Makes
    /// sure the empty-for-next heuristic flips correctly when the
    /// shape switches mid-path.
    #[test]
    fn set_value_scaffolds_mapping_after_index_when_next_is_key() {
        let mut root: Value = serde_yaml_ng::from_str("name: demo\n").unwrap();
        let path = parse_path("foo[0].name").unwrap();
        set_value(&mut root, &path, Value::String("bar".into())).unwrap();

        let foo = root
            .as_mapping()
            .unwrap()
            .get(Value::String("foo".into()))
            .unwrap();
        let outer = foo.as_sequence().expect("foo must be a sequence");
        assert_eq!(outer.len(), 1);
        let elem = outer[0].as_mapping().expect("foo[0] must be a mapping");
        let name = elem.get(Value::String("name".into())).unwrap();
        assert_eq!(name.as_str(), Some("bar"));
    }

    #[test]
    fn set_value_refuses_to_descend_into_scalar() {
        let mut root = sample();
        let path = parse_path("description.foo").unwrap();
        let err = set_value(&mut root, &path, Value::String("x".into())).unwrap_err();
        assert!(matches!(
            err,
            PathError::DescendScalar(_) | PathError::KeyOnSequence { .. }
        ));
    }

    #[test]
    fn delete_value_removes_existing_key() {
        let mut root = sample();
        let path = parse_path("description").unwrap();
        assert_eq!(
            delete_value(&mut root, &path).unwrap(),
            DeleteOutcome::Removed
        );
        assert!(get_value(&root, &path).is_none());
    }

    #[test]
    fn delete_value_removes_existing_index_and_shifts_remaining() {
        let mut root = sample();
        let path = parse_path("dependencies.skills[0]").unwrap();
        assert_eq!(
            delete_value(&mut root, &path).unwrap(),
            DeleteOutcome::Removed
        );
        // What was [1] is now [0].
        let p0 = parse_path("dependencies.skills[0]").unwrap();
        assert_eq!(get_value(&root, &p0).unwrap().as_str(), Some("carol/dave"));
        // And the sequence shrank.
        let p1 = parse_path("dependencies.skills[1]").unwrap();
        assert!(get_value(&root, &p1).is_none());
    }

    #[test]
    fn delete_value_returns_not_present_for_missing_path() {
        let mut root = sample();
        let path = parse_path("missing.deep.key").unwrap();
        assert_eq!(
            delete_value(&mut root, &path).unwrap(),
            DeleteOutcome::NotPresent
        );
    }

    #[test]
    fn delete_value_returns_not_present_for_out_of_bounds_index() {
        let mut root = sample();
        let path = parse_path("dependencies.skills[99]").unwrap();
        assert_eq!(
            delete_value(&mut root, &path).unwrap(),
            DeleteOutcome::NotPresent
        );
    }

    #[test]
    fn get_value_json_resolves_keys_indices_and_scalars() {
        let root: serde_json::Value = serde_json::json!({
            "id": "alice/hello",
            "description": "a demo",
            "versions": [
                {"version": "0.1.0", "sha256": "aaa"},
                {"version": "0.1.1", "sha256": "bbb"},
            ],
        });
        let p = parse_path("description").unwrap();
        assert_eq!(get_value_json(&root, &p).unwrap().as_str(), Some("a demo"));

        let p = parse_path("versions[1].version").unwrap();
        assert_eq!(get_value_json(&root, &p).unwrap().as_str(), Some("0.1.1"));

        let p = parse_path("versions[0]").unwrap();
        assert!(get_value_json(&root, &p).unwrap().is_object());

        let p = parse_path("versions").unwrap();
        assert!(get_value_json(&root, &p).unwrap().is_array());
    }

    #[test]
    fn get_value_json_returns_none_on_miss() {
        let root: serde_json::Value = serde_json::json!({
            "id": "alice/hello",
            "versions": [{"version": "0.1.0"}],
        });
        // Missing top-level key.
        let p = parse_path("nope").unwrap();
        assert!(get_value_json(&root, &p).is_none());
        // Out-of-bounds index.
        let p = parse_path("versions[99]").unwrap();
        assert!(get_value_json(&root, &p).is_none());
        // Descending into a scalar via a key segment.
        let p = parse_path("id.deep").unwrap();
        assert!(get_value_json(&root, &p).is_none());
    }
}
