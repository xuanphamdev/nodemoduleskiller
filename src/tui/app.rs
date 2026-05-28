//! TUI application state and pure-function reducer.
//!
//! Keeps render code and event handling decoupled: the main loop
//! ([`super::run`]) translates `KeyEvent` / scan-result / size-result
//! signals into [`Action`]s; this module applies them to [`AppState`] and
//! returns a small set of side effects that the loop performs (e.g. "delete
//! folder at index N").
//!
//! Why a reducer-style design? It makes the UI logic unit-testable without a
//! real terminal — see the `tests` module below.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::core::sort::sort_results;
use crate::core::types::{FolderResult, ScanFoundFolder, SortBy, SortDirection};

/// What the TUI is currently doing. Most of the time it's `Browse`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// Awaiting Y/N on deleting the row at this cursor index.
    Confirm(usize),
    /// Delete confirmed; waiting for the filesystem operation to complete.
    /// The modal stays open with a spinner so the user knows the app is
    /// working — large `node_modules` trees can take several seconds.
    Deleting(usize),
}

/// All UI-visible state.
#[derive(Debug)]
pub struct AppState {
    pub root: PathBuf,
    pub targets: Vec<String>,
    pub dry_run: bool,

    pub results: Vec<FolderResult>,
    pub cursor: usize,
    pub mode: Mode,
    pub sort: SortBy,
    pub sort_direction: SortDirection,

    /// Set true when the scanner channel closes — used in the status bar.
    pub scan_finished: bool,

    /// `true` once the user has pressed ↑/↓ at least once. Until then we
    /// keep the cursor pinned to row 0 across re-sorts so the "top hit"
    /// stays visible while results stream in. After the user moves the
    /// cursor we switch to "preserve by path" behaviour.
    pub user_navigated: bool,

    /// Live progress counter — number of directories whose contents have
    /// been read. Sourced from `ScanStats::completed` and refreshed by the
    /// main loop on each tick.
    pub dirs_scanned: u64,

    /// Last status / error message shown to the user. Cleared on next action.
    pub last_message: Option<String>,
}

impl AppState {
    /// Create a new state with the default sort (`Size` desc).
    pub fn new(root: PathBuf, targets: Vec<String>, dry_run: bool, sort: SortBy) -> Self {
        Self::with_sort(root, targets, dry_run, sort, SortDirection::default())
    }

    pub fn with_sort(
        root: PathBuf,
        targets: Vec<String>,
        dry_run: bool,
        sort: SortBy,
        sort_direction: SortDirection,
    ) -> Self {
        Self {
            root,
            targets,
            dry_run,
            results: Vec::new(),
            cursor: 0,
            mode: Mode::Browse,
            sort,
            sort_direction,
            scan_finished: false,
            user_navigated: false,
            dirs_scanned: 0,
            last_message: None,
        }
    }

    /// Total size across all rows that have a known size — backwards compat
    /// alias for [`releasable_bytes`] + [`saved_bytes`].
    pub fn total_size(&self) -> u64 {
        self.results.iter().filter_map(|r| r.size_bytes).sum()
    }

    /// Bytes still on disk that the user could reclaim by deleting them.
    /// Excludes rows already deleted in this session.
    pub fn releasable_bytes(&self) -> u64 {
        self.results.iter().filter(|r| !r.deleted).filter_map(|r| r.size_bytes).sum()
    }

    /// Bytes the user has actually reclaimed (or simulated reclaiming, in
    /// dry-run mode) during this session.
    pub fn saved_bytes(&self) -> u64 {
        self.results.iter().filter(|r| r.deleted).filter_map(|r| r.size_bytes).sum()
    }

    /// Currently-highlighted row, if any.
    pub fn selected(&self) -> Option<&FolderResult> {
        self.results.get(self.cursor)
    }
}

/// User intent. Translated by `apply` into state changes + optional side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    /// Open the confirm prompt for the currently-selected row.
    RequestDelete,
    ConfirmYes,
    ConfirmNo,
    /// Toggle sort by size. Pressing again flips direction. When switching
    /// FROM another sort, the direction resets to the default for that key
    /// (Size→Desc, Name→Asc, LastUsed→Desc).
    ToggleSortBySize,
    ToggleSortByName,
    ToggleSortByLastUsed,
    /// Cancel the current scan, clear results, and start a fresh scan with
    /// the same options. Useful after the user has just deleted folders and
    /// wants a clean state.
    Rescan,
    Quit,
    Noop,
}

/// Things the reducer asks the main loop to do that aren't pure state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Perform the actual delete on the FS, then call back into the reducer
    /// via `record_delete_outcome`.
    DeleteFolder {
        index: usize,
        path: PathBuf,
    },
    /// Tear down the TUI and exit.
    Quit,
    /// Cancel the current scan and start a new one with the same options.
    /// Main loop is responsible for the actual scanner lifecycle.
    Rescan,
    None,
}

impl AppState {
    /// Apply an action, returning any side effect for the main loop to execute.
    pub fn apply(&mut self, action: Action) -> Effect {
        self.last_message = None;
        match action {
            Action::Up => {
                self.user_navigated = true;
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                Effect::None
            }
            Action::Down => {
                self.user_navigated = true;
                if !self.results.is_empty() && self.cursor + 1 < self.results.len() {
                    self.cursor += 1;
                }
                Effect::None
            }
            Action::RequestDelete => {
                if self.mode == Mode::Browse
                    && let Some(r) = self.results.get(self.cursor)
                    && !r.deleted
                {
                    self.mode = Mode::Confirm(self.cursor);
                }
                Effect::None
            }
            Action::ConfirmYes => match self.mode.clone() {
                Mode::Confirm(idx) => {
                    if let Some(r) = self.results.get(idx) {
                        // Switch to Deleting so the modal stays open with a
                        // spinner until `record_delete_outcome` lands.
                        self.mode = Mode::Deleting(idx);
                        Effect::DeleteFolder { index: idx, path: r.path.clone() }
                    } else {
                        self.mode = Mode::Browse;
                        Effect::None
                    }
                }
                _ => Effect::None,
            },
            Action::ConfirmNo => {
                if matches!(self.mode, Mode::Confirm(_)) {
                    self.mode = Mode::Browse;
                }
                Effect::None
            }
            Action::ToggleSortBySize => {
                self.toggle_or_switch_sort(SortBy::Size, SortDirection::Desc);
                Effect::None
            }
            Action::ToggleSortByName => {
                self.toggle_or_switch_sort(SortBy::Path, SortDirection::Asc);
                Effect::None
            }
            Action::ToggleSortByLastUsed => {
                self.toggle_or_switch_sort(SortBy::Age, SortDirection::Desc);
                Effect::None
            }
            Action::Rescan => {
                if matches!(self.mode, Mode::Browse) {
                    self.clear_for_rescan();
                    Effect::Rescan
                } else {
                    Effect::None
                }
            }
            Action::Quit => Effect::Quit,
            Action::Noop => Effect::None,
        }
    }

    /// Reset everything that depends on a specific scan run. Keeps the
    /// user's chosen sort + direction + targets + root + dry_run flag.
    pub fn clear_for_rescan(&mut self) {
        self.results.clear();
        self.cursor = 0;
        self.user_navigated = false;
        self.scan_finished = false;
        self.dirs_scanned = 0;
        self.last_message = Some("rescanning…".into());
    }

    fn toggle_or_switch_sort(&mut self, by: SortBy, default_direction: SortDirection) {
        if self.sort == by {
            self.sort_direction = self.sort_direction.toggle();
        } else {
            self.sort = by;
            self.sort_direction = default_direction;
        }
        self.resort();
    }

    /// Sort `results` by the current `sort` + `sort_direction`.
    ///
    /// Cursor policy:
    /// - If the user has not navigated yet (`!user_navigated`), pin to row 0
    ///   so the top hit stays visible while results stream in or get re-sorted.
    /// - Otherwise, preserve the cursor on whichever row was selected
    ///   (by path), so a sort-toggle keeps your item in view.
    /// - Final clamp to keep the cursor in range.
    pub fn resort(&mut self) {
        let selected_path =
            if self.user_navigated { self.selected().map(|r| r.path.clone()) } else { None };
        sort_results(&mut self.results, self.sort, self.sort_direction);
        if let Some(p) = selected_path
            && let Some(idx) = self.results.iter().position(|r| r.path == p)
        {
            self.cursor = idx;
        } else if !self.user_navigated {
            self.cursor = 0;
        } else if self.cursor >= self.results.len() && !self.results.is_empty() {
            self.cursor = self.results.len() - 1;
        }
    }

    /// Push a result coming off the scanner channel. Caller can preset
    /// `last_modified` if they have it (the TUI does this synchronously
    /// from `fs::metadata` so the `Age` sort is meaningful immediately).
    pub fn push_result(&mut self, found: ScanFoundFolder) {
        self.results.push(FolderResult::from_scan(found));
        self.resort();
    }

    /// Push a result and seed its `last_modified` in one call.
    pub fn push_result_with_mtime(
        &mut self,
        found: ScanFoundFolder,
        last_modified: Option<SystemTime>,
    ) {
        let mut row = FolderResult::from_scan(found);
        row.last_modified = last_modified;
        self.results.push(row);
        self.resort();
    }

    /// Update a row's size after the size-calc task completes.
    pub fn record_size(&mut self, path: &std::path::Path, size: u64) {
        let changed = if let Some(row) = self.results.iter_mut().find(|r| r.path == path) {
            row.size_bytes = Some(size);
            true
        } else {
            false
        };
        // Only re-sort if the change can affect ordering.
        if changed && self.sort == SortBy::Size {
            self.resort();
        }
    }

    /// Update a row after a delete attempt completes.
    ///
    /// - **Real-delete success**: remove the row entirely so the user sees
    ///   the freed space drop off the list. Cursor is clamped if needed.
    /// - **Dry-run success**: keep the row but mark `deleted` so the
    ///   strike-through ✗ icon shows; the user can still see what would
    ///   have been removed.
    /// - **Failure**: keep the row exactly as it was, surface the error
    ///   message in the status bar.
    ///
    /// Always closes the modal (Mode → Browse) so the user can keep going.
    pub fn record_delete_outcome(&mut self, index: usize, success: bool, error: Option<String>) {
        self.mode = Mode::Browse;
        if success {
            if self.dry_run {
                if let Some(row) = self.results.get_mut(index) {
                    row.deleted = true;
                }
                self.last_message = Some("(dry-run) would have deleted".into());
            } else if index < self.results.len() {
                self.results.remove(index);
                if !self.results.is_empty() && self.cursor >= self.results.len() {
                    self.cursor = self.results.len() - 1;
                }
                self.last_message = Some("deleted".into());
            }
        } else {
            let msg = error.unwrap_or_else(|| "unknown error".into());
            self.last_message = Some(format!("delete failed: {msg}"));
        }
    }

    /// Called when the scanner channel closes. Per user request: always
    /// snap the cursor to the first row when the scan completes — even if
    /// the user has been navigating — so the "top hit" is immediately
    /// visible. The user can still scroll afterwards.
    pub fn mark_scan_finished(&mut self) {
        let was_already_finished = self.scan_finished;
        self.scan_finished = true;
        if !was_already_finished && !self.results.is_empty() {
            self.cursor = 0;
            self.user_navigated = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn fresh_state() -> AppState {
        AppState::new(PathBuf::from("/root"), vec!["node_modules".into()], false, SortBy::Size)
    }

    fn push(state: &mut AppState, p: &str) {
        state.push_result(ScanFoundFolder::new(PathBuf::from(p), None));
    }

    #[test]
    fn default_sort_is_size_desc() {
        let s = fresh_state();
        assert_eq!(s.sort, SortBy::Size);
        assert_eq!(s.sort_direction, SortDirection::Desc);
    }

    #[test]
    fn navigation_stays_in_bounds() {
        let mut s = fresh_state();
        push(&mut s, "/a");
        push(&mut s, "/b");
        push(&mut s, "/c");
        assert_eq!(s.cursor, 0);
        s.apply(Action::Up);
        assert_eq!(s.cursor, 0); // can't go below 0
        s.apply(Action::Down);
        s.apply(Action::Down);
        s.apply(Action::Down); // tries to go past last
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn delete_flow_yes_emits_effect_and_stays_in_deleting_until_outcome() {
        let mut s = fresh_state();
        push(&mut s, "/a/node_modules");
        s.apply(Action::RequestDelete);
        assert_eq!(s.mode, Mode::Confirm(0));
        let eff = s.apply(Action::ConfirmYes);
        match eff {
            Effect::DeleteFolder { index, path } => {
                assert_eq!(index, 0);
                assert_eq!(path, PathBuf::from("/a/node_modules"));
            }
            other => panic!("expected DeleteFolder, got {other:?}"),
        }
        // Modal stays open with the spinner while the FS op runs.
        assert_eq!(s.mode, Mode::Deleting(0));
        // Outcome arrives — modal closes, real-delete success removes the row.
        s.record_delete_outcome(0, true, None);
        assert_eq!(s.mode, Mode::Browse);
        assert!(s.results.is_empty(), "real-delete success should drop the row");
        assert_eq!(s.last_message.as_deref(), Some("deleted"));
    }

    #[test]
    fn delete_failure_keeps_row_and_surfaces_error() {
        let mut s = fresh_state();
        push(&mut s, "/a/node_modules");
        s.apply(Action::RequestDelete);
        s.apply(Action::ConfirmYes);
        assert_eq!(s.mode, Mode::Deleting(0));
        s.record_delete_outcome(0, false, Some("permission denied".into()));
        assert_eq!(s.mode, Mode::Browse);
        assert_eq!(s.results.len(), 1, "failed delete must leave the row visible");
        assert!(
            s.last_message.as_deref().unwrap().contains("permission denied"),
            "got {:?}",
            s.last_message
        );
    }

    #[test]
    fn dry_run_keeps_row_and_marks_deleted_visually() {
        let mut s = fresh_state();
        s.dry_run = true;
        push(&mut s, "/a/node_modules");
        s.apply(Action::RequestDelete);
        s.apply(Action::ConfirmYes);
        s.record_delete_outcome(0, true, None);
        assert_eq!(s.results.len(), 1, "dry-run never deletes; row stays");
        assert!(s.results[0].deleted, "dry-run still marks the row visually");
        assert!(s.last_message.as_deref().unwrap().contains("dry-run"));
    }

    #[test]
    fn cursor_clamps_after_removing_last_row() {
        let mut s = fresh_state();
        push(&mut s, "/a/node_modules");
        push(&mut s, "/b/node_modules");
        push(&mut s, "/c/node_modules");
        // Move cursor to the last row.
        s.apply(Action::Down);
        s.apply(Action::Down);
        assert_eq!(s.cursor, 2);
        // Delete it.
        s.apply(Action::RequestDelete);
        s.apply(Action::ConfirmYes);
        s.record_delete_outcome(2, true, None);
        assert_eq!(s.results.len(), 2);
        assert_eq!(s.cursor, 1, "cursor should clamp to the new last row");
    }

    #[test]
    fn delete_flow_no_returns_to_browse_without_effect() {
        let mut s = fresh_state();
        push(&mut s, "/a/node_modules");
        s.apply(Action::RequestDelete);
        assert_eq!(s.mode, Mode::Confirm(0));
        let eff = s.apply(Action::ConfirmNo);
        assert_eq!(eff, Effect::None);
        assert_eq!(s.mode, Mode::Browse);
    }

    #[test]
    fn cannot_request_delete_when_no_rows() {
        let mut s = fresh_state();
        s.apply(Action::RequestDelete);
        assert_eq!(s.mode, Mode::Browse);
    }

    #[test]
    fn cannot_redelete_a_dryrun_deleted_row() {
        // Real-delete now removes the row entirely; the only way a row can
        // be visible AND flagged `deleted` is in dry-run mode.
        let mut s = fresh_state();
        s.dry_run = true;
        push(&mut s, "/a/node_modules");
        s.record_delete_outcome(0, true, None);
        assert!(s.results[0].deleted, "precondition: dry-run leaves row marked");
        s.apply(Action::RequestDelete);
        assert_eq!(s.mode, Mode::Browse, "should not open confirm for an already-marked row");
    }

    #[test]
    fn record_size_updates_matching_row() {
        let mut s = fresh_state();
        push(&mut s, "/a/node_modules");
        push(&mut s, "/b/node_modules");
        s.record_size(Path::new("/a/node_modules"), 42_000);
        let row_a = s.results.iter().find(|r| r.path == Path::new("/a/node_modules")).unwrap();
        let row_b = s.results.iter().find(|r| r.path == Path::new("/b/node_modules")).unwrap();
        assert_eq!(row_a.size_bytes, Some(42_000));
        assert_eq!(row_b.size_bytes, None);
    }

    #[test]
    fn total_size_sums_known_sizes() {
        let mut s = fresh_state();
        push(&mut s, "/a");
        push(&mut s, "/b");
        s.record_size(Path::new("/a"), 1_000);
        s.record_size(Path::new("/b"), 2_500);
        assert_eq!(s.total_size(), 3_500);
    }

    #[test]
    fn quit_returns_quit_effect() {
        let mut s = fresh_state();
        assert_eq!(s.apply(Action::Quit), Effect::Quit);
    }

    #[test]
    fn dry_run_message_after_delete() {
        let mut s = fresh_state();
        s.dry_run = true;
        push(&mut s, "/a/node_modules");
        s.record_delete_outcome(0, true, None);
        assert!(s.last_message.as_deref().unwrap().contains("dry-run"));
    }

    // ─── Sort toggle behaviour ──────────────────────────────────────────────

    #[test]
    fn pressing_size_again_flips_direction() {
        let mut s = fresh_state();
        assert_eq!(s.sort_direction, SortDirection::Desc);
        s.apply(Action::ToggleSortBySize);
        assert_eq!(s.sort, SortBy::Size);
        assert_eq!(s.sort_direction, SortDirection::Asc);
        s.apply(Action::ToggleSortBySize);
        assert_eq!(s.sort_direction, SortDirection::Desc);
    }

    #[test]
    fn switching_from_size_to_name_uses_default_asc() {
        let mut s = fresh_state(); // Size + Desc
        s.apply(Action::ToggleSortByName);
        assert_eq!(s.sort, SortBy::Path);
        assert_eq!(s.sort_direction, SortDirection::Asc);
    }

    #[test]
    fn switching_to_last_used_uses_default_desc() {
        let mut s = fresh_state();
        s.apply(Action::ToggleSortByName); // somewhere else
        s.apply(Action::ToggleSortByLastUsed);
        assert_eq!(s.sort, SortBy::Age);
        assert_eq!(s.sort_direction, SortDirection::Desc);
        // Toggling again flips.
        s.apply(Action::ToggleSortByLastUsed);
        assert_eq!(s.sort_direction, SortDirection::Asc);
    }

    #[test]
    fn resort_keeps_cursor_on_same_row_after_user_navigates() {
        let mut s = fresh_state(); // sort=Size desc
        push(&mut s, "/aaa");
        push(&mut s, "/bbb");
        push(&mut s, "/ccc");
        s.record_size(Path::new("/aaa"), 100);
        s.record_size(Path::new("/bbb"), 999);
        s.record_size(Path::new("/ccc"), 500);
        // Order under Size+Desc: bbb(999), ccc(500), aaa(100).
        // Move cursor down to the middle (ccc) — this also flips user_navigated.
        s.apply(Action::Down);
        assert!(s.user_navigated);
        let ccc_idx = s.results.iter().position(|r| r.path == Path::new("/ccc")).unwrap();
        assert_eq!(s.cursor, ccc_idx);
        // Flip to Asc.
        s.apply(Action::ToggleSortBySize);
        // New order: aaa(100), ccc(500), bbb(999). Cursor should still point to ccc.
        let new_idx = s.results.iter().position(|r| r.path == Path::new("/ccc")).unwrap();
        assert_eq!(s.cursor, new_idx);
    }

    // ─── Auto-top + rescan behaviour ────────────────────────────────────────

    #[test]
    fn cursor_pinned_at_top_during_streaming_until_user_navigates() {
        // Simulate live streaming: rows arrive one-by-one with sizes filling in.
        let mut s = fresh_state();
        let scan = |p: &str| ScanFoundFolder::new(PathBuf::from(p), None);
        s.push_result(scan("/small"));
        s.record_size(Path::new("/small"), 100);
        // Now a bigger row arrives — it should sort to the top under Size+Desc,
        // and because user hasn't navigated, the cursor must move to the new top row.
        s.push_result(scan("/big"));
        s.record_size(Path::new("/big"), 10_000);
        assert_eq!(s.cursor, 0);
        assert_eq!(s.selected().unwrap().path, PathBuf::from("/big"));

        // User presses Down → user_navigated flips → cursor follows the row.
        s.apply(Action::Down);
        assert!(s.user_navigated);
        let small_idx = s.results.iter().position(|r| r.path == Path::new("/small")).unwrap();
        assert_eq!(s.cursor, small_idx);

        // Another row arrives mid-stream; cursor should STAY on /small now.
        s.push_result(scan("/medium"));
        s.record_size(Path::new("/medium"), 1_000);
        assert_eq!(s.selected().unwrap().path, PathBuf::from("/small"));
    }

    #[test]
    fn scan_finished_snaps_cursor_to_top_even_if_user_navigated() {
        let mut s = fresh_state();
        push(&mut s, "/a");
        push(&mut s, "/b");
        push(&mut s, "/c");
        // User moved around mid-scan.
        s.apply(Action::Down);
        s.apply(Action::Down);
        assert!(s.cursor > 0);
        // Scan completes.
        s.mark_scan_finished();
        assert_eq!(s.cursor, 0, "post-scan cursor must snap to top");
        assert!(s.scan_finished);
        // user_navigated also resets so further streaming-style pushes pin to top
        // (in case the user later rescans).
        assert!(!s.user_navigated);
    }

    #[test]
    fn scan_finished_is_idempotent_does_not_reset_cursor_on_redundant_calls() {
        let mut s = fresh_state();
        push(&mut s, "/a");
        push(&mut s, "/b");
        push(&mut s, "/c");
        s.mark_scan_finished();
        assert_eq!(s.cursor, 0);
        // Now user navigates AFTER scan finished.
        s.apply(Action::Down);
        assert_eq!(s.cursor, 1);
        // A second mark_scan_finished call (e.g., main loop double-checks) must
        // NOT yank the cursor back to 0 again.
        s.mark_scan_finished();
        assert_eq!(s.cursor, 1, "redundant mark_scan_finished should be a no-op");
    }

    #[test]
    fn rescan_clears_results_and_emits_effect() {
        let mut s = fresh_state();
        push(&mut s, "/a");
        push(&mut s, "/b");
        s.apply(Action::Down);
        s.mark_scan_finished();
        // Sanity.
        assert_eq!(s.results.len(), 2);
        assert!(s.scan_finished);

        let eff = s.apply(Action::Rescan);
        assert_eq!(eff, Effect::Rescan);
        assert!(s.results.is_empty());
        assert_eq!(s.cursor, 0);
        assert!(!s.scan_finished);
        assert!(!s.user_navigated);
        // status message acknowledges the action
        assert!(s.last_message.as_deref().unwrap_or("").contains("rescan"));
    }

    #[test]
    fn rescan_ignored_in_confirm_mode() {
        let mut s = fresh_state();
        push(&mut s, "/a/node_modules");
        s.apply(Action::RequestDelete);
        assert!(matches!(s.mode, Mode::Confirm(_)));
        let eff = s.apply(Action::Rescan);
        assert_eq!(eff, Effect::None);
        // Modal is still active and results unchanged.
        assert!(matches!(s.mode, Mode::Confirm(_)));
        assert_eq!(s.results.len(), 1);
    }

    #[test]
    fn push_keeps_results_sorted() {
        let mut s = fresh_state();
        // Push in random order with sizes preset via push then record_size.
        let scan = |p: &str| ScanFoundFolder::new(PathBuf::from(p), None);
        s.push_result(scan("/small"));
        s.record_size(Path::new("/small"), 100);
        s.push_result(scan("/big"));
        s.record_size(Path::new("/big"), 10_000);
        s.push_result(scan("/medium"));
        s.record_size(Path::new("/medium"), 1_000);
        // Default Size + Desc → big, medium, small.
        let paths: Vec<_> =
            s.results.iter().map(|r| r.path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, vec!["/big", "/medium", "/small"]);
    }

    #[test]
    fn age_sort_uses_last_modified() {
        let mut s = fresh_state();
        let now = SystemTime::now();
        s.push_result_with_mtime(
            ScanFoundFolder::new(PathBuf::from("/recent"), None),
            Some(now - Duration::from_secs(10)),
        );
        s.push_result_with_mtime(
            ScanFoundFolder::new(PathBuf::from("/ancient"), None),
            Some(now - Duration::from_secs(10_000)),
        );
        s.push_result_with_mtime(ScanFoundFolder::new(PathBuf::from("/no-mtime"), None), None);
        s.apply(Action::ToggleSortByLastUsed); // Age + Desc (newest first)
        let paths: Vec<_> =
            s.results.iter().map(|r| r.path.to_string_lossy().into_owned()).collect();
        assert_eq!(paths, vec!["/recent", "/ancient", "/no-mtime"]);
    }
}
