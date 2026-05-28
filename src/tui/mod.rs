//! Interactive ratatui-based UI for the `cft` binary.
//!
//! v0.1 ships a minimal feel: header, results table, status hint, delete
//! confirm modal, and **interactive sort** via `s` / `n` / `m` keys.
//!
//! The main loop drives three event sources at the same time:
//! 1. periodic tick (so the scan-status spinner and pending sizes refresh),
//! 2. crossterm `EventStream` (key + resize),
//! 3. the scanner's result channel + the size-result channel.
//!
//! Terminal state is owned by [`TerminalGuard`] — a RAII handle that always
//! restores the terminal on drop, including panic unwind.

pub mod app;
pub mod render;

use std::collections::HashSet;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::cli::CliArgs;
use crate::core::types::{ScanOptions, SortBy};
use crate::core::{delete, risk, scanner, size};

use self::app::{Action, AppState, Effect, Mode};

/// RAII terminal-mode guard: restores the terminal on drop, even during panic.
struct TerminalGuard {
    term: Option<Terminal<CrosstermBackend<Stdout>>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen)?;
        let term = Terminal::new(CrosstermBackend::new(out))?;
        Ok(Self { term: Some(term) })
    }

    fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        self.term.as_mut().expect("terminal taken")
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some(mut term) = self.term.take() {
            let _ = disable_raw_mode();
            let _ = execute!(term.backend_mut(), LeaveAlternateScreen);
            let _ = term.show_cursor();
        }
    }
}

/// Entry point invoked by `main.rs` when stdout is a TTY and `--no-tui` was not passed.
pub async fn run(args: CliArgs) -> anyhow::Result<()> {
    let root = args.root_path()?;
    let targets = args.resolved_targets();
    let sort: SortBy = args.sort.into();
    let dry_run = args.dry_run;

    // ScanOptions are rebuilt for every (re-)scan via `make_opts`.
    let make_opts = || ScanOptions {
        targets: targets.clone(),
        exclude: args.exclude.clone(),
        sort_by: Some(sort),
        perform_risk_analysis: !args.no_risk,
    };

    // Default direction comes from `SortDirection::default()` = Desc, which
    // happens to match the user's expectation for the default Size sort.
    let mut state = AppState::new(root.clone(), targets.clone(), dry_run, sort);

    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok();
    let home_path = home.as_deref().map(std::path::PathBuf::from);

    let mut handle = scanner::start_scan(root.clone(), make_opts());

    let mut term = TerminalGuard::enter().context("failed to enter TUI mode")?;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));

    // Size-calculation pipeline: rows we've already requested size for, plus
    // a channel for completed sizes.
    let mut size_requested: HashSet<PathBuf> = HashSet::new();
    let (size_tx, mut size_rx) = mpsc::channel::<(PathBuf, u64)>(256);

    // Delete-outcome pipeline.
    let (del_tx, mut del_rx) = mpsc::channel::<(usize, bool, Option<String>)>(32);

    loop {
        // Draw current state.
        term.terminal().draw(|f| render::draw(f, &state))?;

        tokio::select! {
            _ = tick.tick() => {
                // Refresh live progress counter from the scanner.
                state.dirs_scanned =
                    handle.stats.completed.load(std::sync::atomic::Ordering::Relaxed);

                // Kick off size requests for any new rows we haven't asked about yet.
                for r in &state.results {
                    if r.size_bytes.is_none() && size_requested.insert(r.path.clone()) {
                        let p = r.path.clone();
                        let tx = size_tx.clone();
                        tokio::spawn(async move {
                            let s = size::get_folder_size(p.clone()).await.unwrap_or(0);
                            let _ = tx.send((p, s)).await;
                        });
                    }
                }
            }

            Some(ev) = events.next() => {
                let Ok(ev) = ev else { continue };
                if let Event::Key(k) = ev {
                    let action = map_key(k, &state.mode);
                    let effect = state.apply(action);
                    match effect {
                        Effect::Quit => {
                            handle.cancel.cancel();
                            break;
                        }
                        Effect::DeleteFolder { index, path } => {
                            let scan_root = state.root.clone();
                            let targets = state.targets.clone();
                            let tx = del_tx.clone();
                            tokio::spawn(async move {
                                let res = delete::delete(&path, &scan_root, &targets, dry_run).await;
                                let _ = tx.send((index, res.success, res.error)).await;
                            });
                        }
                        Effect::Rescan => {
                            // Cancel current scan; the channel will drain to None on the
                            // next iteration. Reset per-scan caches.
                            handle.cancel.cancel();
                            size_requested.clear();
                            // Spawn a fresh scanner. AppState was already cleared by the
                            // reducer (`clear_for_rescan`).
                            handle = scanner::start_scan(root.clone(), make_opts());
                        }
                        Effect::None => {}
                    }
                }
            }

            Some(found) = handle.results.recv() => {
                let mut found = found;
                // Replace the scanner's placeholder safe-by-default with a real
                // risk analysis using the cached home dir.
                if !args.no_risk {
                    found.risk_analysis = Some(risk::analyze_with_home(&found.path, home_path.as_deref()));
                }
                // Synchronously stat the folder so the Age sort works as soon
                // as the row appears. Single syscall — cheap.
                let mtime = std::fs::metadata(&found.path).and_then(|m| m.modified()).ok();
                state.push_result_with_mtime(found, mtime);
            }

            Some((path, sz)) = size_rx.recv() => {
                state.record_size(&path, sz);
            }

            Some((idx, ok, err)) = del_rx.recv() => {
                state.record_delete_outcome(idx, ok, err);
            }

            else => {
                if !state.scan_finished {
                    state.mark_scan_finished();
                }
                tokio::time::sleep(Duration::from_millis(120)).await;
            }
        }

        // Detect "scan done" via EITHER the channel closing OR the cancel
        // token firing (the scanner cancels itself when pending hits 0).
        // Checking both gives belt-and-suspenders coverage: the channel can
        // remain open briefly while in-flight workers exit, but cancel
        // reflects logical completion immediately.
        if !state.scan_finished && (handle.cancel.is_cancelled() || handle.results.is_closed()) {
            state.mark_scan_finished();
        }
    }

    Ok(())
}

/// Translate a key press into an [`Action`]. Mode-aware: in `Confirm`, only
/// y/n/Esc/Ctrl-C are meaningful so the same physical keys can serve other
/// purposes in `Browse` (e.g. `n` toggles sort-by-name).
fn map_key(k: KeyEvent, mode: &Mode) -> Action {
    if k.kind == KeyEventKind::Release {
        return Action::Noop;
    }
    // Ctrl-C always quits, regardless of mode.
    if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c')) {
        return Action::Quit;
    }
    if matches!(mode, Mode::Confirm(_)) {
        return match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Action::ConfirmYes,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::ConfirmNo,
            _ => Action::Noop,
        };
    }
    // Lock the keyboard while a delete is in flight. Only Ctrl-C (handled
    // above) can exit. We don't allow cancelling mid-delete because
    // `std::fs::remove_dir_all` has no clean abort path — half-deleted
    // trees would be worse than waiting.
    if matches!(mode, Mode::Deleting(_)) {
        return Action::Noop;
    }
    // Browse mode.
    match k.code {
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('d') | KeyCode::Char(' ') | KeyCode::Enter => Action::RequestDelete,
        KeyCode::Char('s') | KeyCode::Char('S') => Action::ToggleSortBySize,
        KeyCode::Char('n') | KeyCode::Char('N') => Action::ToggleSortByName,
        KeyCode::Char('m') | KeyCode::Char('M') => Action::ToggleSortByLastUsed,
        KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::F(5) => Action::Rescan,
        _ => Action::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn browse_mode_navigation_and_delete() {
        let m = Mode::Browse;
        assert_eq!(map_key(key(KeyCode::Up), &m), Action::Up);
        assert_eq!(map_key(key(KeyCode::Char('k')), &m), Action::Up);
        assert_eq!(map_key(key(KeyCode::Down), &m), Action::Down);
        assert_eq!(map_key(key(KeyCode::Char('j')), &m), Action::Down);
        assert_eq!(map_key(key(KeyCode::Char('q')), &m), Action::Quit);
        assert_eq!(map_key(key(KeyCode::Char('d')), &m), Action::RequestDelete);
        assert_eq!(map_key(key(KeyCode::Enter), &m), Action::RequestDelete);
        assert_eq!(map_key(key(KeyCode::Char(' ')), &m), Action::RequestDelete);
    }

    #[test]
    fn browse_mode_sort_keys() {
        let m = Mode::Browse;
        assert_eq!(map_key(key(KeyCode::Char('s')), &m), Action::ToggleSortBySize);
        assert_eq!(map_key(key(KeyCode::Char('n')), &m), Action::ToggleSortByName);
        assert_eq!(map_key(key(KeyCode::Char('m')), &m), Action::ToggleSortByLastUsed);
    }

    #[test]
    fn browse_mode_rescan_keys() {
        let m = Mode::Browse;
        assert_eq!(map_key(key(KeyCode::Char('r')), &m), Action::Rescan);
        assert_eq!(map_key(key(KeyCode::Char('R')), &m), Action::Rescan);
        assert_eq!(map_key(key(KeyCode::F(5)), &m), Action::Rescan);
    }

    #[test]
    fn confirm_mode_only_yes_no_esc() {
        let m = Mode::Confirm(0);
        assert_eq!(map_key(key(KeyCode::Char('y')), &m), Action::ConfirmYes);
        assert_eq!(map_key(key(KeyCode::Char('Y')), &m), Action::ConfirmYes);
        assert_eq!(map_key(key(KeyCode::Char('n')), &m), Action::ConfirmNo);
        assert_eq!(map_key(key(KeyCode::Char('N')), &m), Action::ConfirmNo);
        assert_eq!(map_key(key(KeyCode::Esc), &m), Action::ConfirmNo);
        // Sort keys are NOT sort actions in Confirm — they're noops.
        assert_eq!(map_key(key(KeyCode::Char('s')), &m), Action::Noop);
        assert_eq!(map_key(key(KeyCode::Char('d')), &m), Action::Noop);
    }

    #[test]
    fn ctrl_c_quits_in_any_mode() {
        let k = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(map_key(k, &Mode::Browse), Action::Quit);
        assert_eq!(map_key(k, &Mode::Confirm(0)), Action::Quit);
    }

    #[test]
    fn unknown_keys_are_noop() {
        let m = Mode::Browse;
        assert_eq!(map_key(key(KeyCode::F(1)), &m), Action::Noop);
        assert_eq!(map_key(key(KeyCode::Char('x')), &m), Action::Noop);
    }

    #[test]
    fn key_release_is_noop() {
        let mut k = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        k.kind = KeyEventKind::Release;
        assert_eq!(map_key(k, &Mode::Browse), Action::Noop);
    }
}
