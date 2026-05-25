//! Per-dependency install progress reporting.
//!
//! The install [`runner`](super::runner) processes each declared
//! dependency sequentially and is otherwise presentation-agnostic — it
//! never decides *how* progress is rendered. This module defines the
//! thin [`ProgressSink`] seam the runner calls at each dep's lifecycle
//! boundary, plus two implementations:
//!
//! * [`NoopSink`] — does nothing. Used by callers that don't want any
//!   UI (the legacy [`runner::run`](super::runner::run) entry point,
//!   `pakx update`'s in-process reconcile, and every test).
//! * [`MultiProgressSink`] — owns an `indicatif::MultiProgress` with one
//!   bar per dependency, each advancing through its lifecycle and
//!   finishing with a success / skip / fail state. Used by
//!   `pakx install`'s human render.
//!
//! The seam carries **only** presentation signals — it must never
//! influence control flow, the install outcome, the lockfile, or the
//! `tracing` log trail. The runner's behaviour is byte-for-byte
//! identical whichever sink it's handed.

use std::sync::Mutex;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Coarse lifecycle phase for one dependency. The runner reports the
/// phase it is *about* to enter; the sink decides whether / how to show
/// it. Phases are intentionally coarse — they map to the boundaries the
/// runner can observe without threading the sink through the deep
/// download / verify / extract code in [`super::skill`] / [`super::bundle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Resolving the canonical id + version through the registry.
    Resolve,
    /// Downloading + verifying + extracting the payload.
    Install,
}

impl Phase {
    /// Human verb shown in the per-dep progress message.
    const fn verb(self) -> &'static str {
        match self {
            Self::Resolve => "resolving",
            Self::Install => "installing",
        }
    }
}

/// Presentation seam the install runner calls at each dependency's
/// lifecycle boundary. Every method takes the dep's display id so an
/// implementation can address the right bar without the runner holding
/// any handle.
///
/// All methods are no-ops by contract for sinks that don't render —
/// the runner must remain correct regardless of what (if anything) the
/// sink does with the signals.
///
/// `Send + Sync` because the runner holds `&dyn ProgressSink` across
/// `.await` points; the tokio multi-thread runtime requires the
/// resulting future be `Send`, which forces the shared reference to be
/// `Sync`. Both implementations satisfy this ([`NoopSink`] is a ZST;
/// [`MultiProgressSink`] guards its bars with a [`Mutex`]).
pub trait ProgressSink: Send + Sync {
    /// Register a dependency about to be processed. Called once per dep
    /// before any [`phase`](Self::phase) / `finish_*` call for that id.
    fn begin(&self, id: &str);
    /// Note the dep is entering `phase`.
    fn phase(&self, id: &str, phase: Phase);
    /// The dep finished successfully (newly installed).
    fn finish_ok(&self, id: &str);
    /// The dep was skipped (already installed / unchanged).
    fn finish_skipped(&self, id: &str);
    /// The dep failed; `reason` is the rendered one-line error.
    fn finish_failed(&self, id: &str, reason: &str);
}

/// A [`ProgressSink`] that renders nothing. The runner's default.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn begin(&self, _id: &str) {}
    fn phase(&self, _id: &str, _phase: Phase) {}
    fn finish_ok(&self, _id: &str) {}
    fn finish_skipped(&self, _id: &str) {}
    fn finish_failed(&self, _id: &str, _reason: &str) {}
}

/// Decide whether per-dep progress bars should render to a real stream
/// or be hidden.
///
/// Mirrors the single-spinner gate in [`crate::ui::spinner`]: progress
/// is *presentation* and must be suppressed whenever the stream isn't an
/// interactive terminal we own. `render` is the resolved decision (e.g.
/// `crate::ui::stderr_progress_enabled()` which folds in `--color
/// never` / `NO_COLOR` / `IsTerminal`). When `false`, every bar is
/// constructed `hidden()` so the bytes never reach stderr — keeping CI
/// logs and `--json` stdout clean.
///
/// Pulled out as a free function so it can be unit-tested without an
/// `indicatif` handle.
#[must_use]
pub const fn should_render(render: bool) -> bool {
    render
}

/// A [`ProgressSink`] backed by an `indicatif::MultiProgress` with one
/// bar per dependency.
///
/// Bars are created **lazily** on first [`begin`](ProgressSink::begin)
/// for a given id and appended to the multi-bar area in arrival order —
/// which, because the runner processes deps in declaration order, is the
/// same order the manifest lists them. The active dep's bar spins
/// through its phases; once it finishes it settles to a terminal
/// `[ok] / [skip] / [fail]` line and the next dep's bar appears below.
/// Lazy creation means the sink needs no up-front dep census, so the
/// command never has to re-read the manifest just to size the UI.
///
/// Each bar is keyed by the dep's display id; lookups are linear over a
/// small vec (a handful of deps), cheaper than a map at this size.
/// Interior mutability ([`Mutex`]) lets the `&self` trait methods append
/// / look up bars; the runner is sequential so contention is nil and the
/// lock is only ever held for the brief bar mutation.
///
/// When [`should_render`] is `false` every bar is `hidden()`, so the
/// sink is a structural no-op on non-TTY / `--json` / `--color never`
/// paths — the runner still calls every method, but nothing reaches the
/// terminal.
pub struct MultiProgressSink {
    multi: MultiProgress,
    bars: Mutex<Vec<(String, ProgressBar)>>,
    style: ProgressStyle,
    render: bool,
}

impl MultiProgressSink {
    /// Build an empty sink; bars are added lazily as the runner reports
    /// each dep. When `render` is `false`, every lazily-added bar is
    /// hidden — see [`should_render`].
    #[must_use]
    pub fn new(render: bool) -> Self {
        let style = ProgressStyle::with_template("{spinner:.cyan.bold} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner());
        Self {
            multi: MultiProgress::new(),
            bars: Mutex::new(Vec::new()),
            style,
            render: should_render(render),
        }
    }

    /// Fetch the bar for `id`, creating + registering it on first sight.
    /// Returns a clone of the `ProgressBar` handle (cheap — `Arc` inside)
    /// so the caller can mutate it without holding the bars lock across
    /// an `indicatif` draw.
    fn bar_for(&self, id: &str) -> ProgressBar {
        let mut bars = self
            .bars
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, pb)) = bars.iter().find(|(bid, _)| bid == id) {
            return pb.clone();
        }
        let pb = if self.render {
            self.multi.add(ProgressBar::new_spinner())
        } else {
            // Hidden bars are still owned by the MultiProgress so the
            // call surface is uniform, but render to nowhere.
            self.multi.add(ProgressBar::hidden())
        };
        pb.set_style(self.style.clone());
        bars.push((id.to_owned(), pb.clone()));
        pb
    }

    /// Test-only: count of bars created so far.
    #[cfg(test)]
    fn bar_count(&self) -> usize {
        self.bars
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl ProgressSink for MultiProgressSink {
    fn begin(&self, id: &str) {
        let pb = self.bar_for(id);
        pb.set_message(format!("{id}: starting"));
        if self.render {
            pb.enable_steady_tick(Duration::from_millis(120));
        }
    }

    fn phase(&self, id: &str, phase: Phase) {
        self.bar_for(id)
            .set_message(format!("{id}: {}", phase.verb()));
    }

    fn finish_ok(&self, id: &str) {
        self.bar_for(id).finish_with_message(format!("[ok] {id}"));
    }

    fn finish_skipped(&self, id: &str) {
        self.bar_for(id).finish_with_message(format!("[skip] {id}"));
    }

    fn finish_failed(&self, id: &str, reason: &str) {
        self.bar_for(id)
            .finish_with_message(format!("[fail] {id}: {reason}"));
    }
}

impl Drop for MultiProgressSink {
    fn drop(&mut self) {
        // Belt-and-suspenders: any bar not explicitly finished (e.g. a
        // dep the runner never reported a terminal state for) is cleared
        // so it can't leave a dangling spinner line. Finished bars are
        // unaffected — `finish_*` already settled them.
        if let Ok(bars) = self.bars.lock() {
            for (_, pb) in bars.iter() {
                if !pb.is_finished() {
                    pb.finish_and_clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_render_is_identity() {
        assert!(should_render(true));
        assert!(!should_render(false));
    }

    #[test]
    fn phase_verbs_are_stable() {
        assert_eq!(Phase::Resolve.verb(), "resolving");
        assert_eq!(Phase::Install.verb(), "installing");
    }

    #[test]
    fn noop_sink_accepts_every_signal() {
        // Compile + run check: the no-op sink must accept the full
        // method surface without panicking. Used by tests + the legacy
        // entry point so it must stay a pure no-op.
        let sink = NoopSink;
        sink.begin("owner/name");
        sink.phase("owner/name", Phase::Resolve);
        sink.phase("owner/name", Phase::Install);
        sink.finish_ok("owner/name");
        sink.finish_skipped("owner/name");
        sink.finish_failed("owner/name", "boom");
    }

    #[test]
    fn hidden_multi_sink_is_a_structural_noop() {
        // With render=false every bar is hidden; driving the full
        // lifecycle must not panic and must not touch a real stream.
        let sink = MultiProgressSink::new(false);
        sink.begin("a/one");
        sink.phase("a/one", Phase::Resolve);
        sink.phase("a/one", Phase::Install);
        sink.finish_ok("a/one");
        sink.begin("b/two");
        sink.finish_failed("b/two", "nope");
        // A terminal call on an id never `begin`-ed lazily creates its
        // bar (tolerated — defensive against ordering assumptions).
        sink.finish_skipped("c/three");
    }

    #[test]
    fn bars_are_created_lazily_per_unique_id() {
        let sink = MultiProgressSink::new(false);
        assert_eq!(sink.bar_count(), 0);
        sink.begin("x/y");
        assert_eq!(sink.bar_count(), 1);
        // Re-touching the same id reuses its bar (no duplicate).
        sink.phase("x/y", Phase::Install);
        sink.finish_ok("x/y");
        assert_eq!(sink.bar_count(), 1);
        // A new id grows the set.
        sink.begin("z/w");
        assert_eq!(sink.bar_count(), 2);
    }
}
