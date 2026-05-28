//! # cruftkill — polyglot dev-cache reaper
//!
//! **Find and delete `node_modules`, `.venv`, `target`, `DerivedData`,
//! `__pycache__`, `obj`, `.gradle`, `.next`, `.turbo`, and the rest of
//! your build cruft from a fast terminal UI.**
//!
//! ```text
//!  ██████╗███████╗████████╗
//! ██╔════╝██╔════╝╚══██╔══╝
//! ██║     █████╗     ██║
//! ██║     ██╔══╝     ██║
//! ╚██████╗██║        ██║
//!  ╚═════╝╚═╝        ╚═╝
//! ```
//!
//! Inspired by [voidcosmos/npkill](https://github.com/voidcosmos/npkill) —
//! rewritten in Rust with a parallel async scanner and extended to **17
//! ecosystems**. Single binary, no runtime dependencies.
//!
//! > ⚠️ `cft` deletes recursively without a recycle bin. Always review the
//! > list before pressing `d`. Run with `--dry-run` first if you want a
//! > preview.
//!
//! ## Quick start
//!
//! ```bash
//! cargo install cruftkill
//!
//! cft                            # scan current dir with `node` profile
//! cft ~/Projects                 # scan a specific directory
//! cft -p rust ~/code             # use the `rust` profile (matches `target/`)
//! cft -p node -p python ~/code   # combine profiles
//! cft -p all ~/                  # every ecosystem at once
//! cft --dry-run ~/Projects       # preview without deleting
//! cft --no-tui ~/Projects | jq   # scriptable NDJSON output
//! ```
//!
//! ## What gets deleted (17 ecosystems)
//!
//! | Profile          | Matches |
//! |------------------|---------|
//! | `node` (default) | `node_modules`, `.npm`, `.pnpm-store`, `.next`, `.nuxt`, `.angular`, `.svelte-kit`, `.vite`, `.nx`, `.turbo`, `.parcel-cache`, `.cache`, `coverage`, `.jest`, … |
//! | `python`         | `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.tox`, `.venv`, `venv`, … |
//! | `rust`           | `target` |
//! | `java`           | `target`, `.gradle`, `out` |
//! | `swift`          | `DerivedData`, `.swiftpm` |
//! | `dotnet`         | `obj`, `TestResults`, `.vs` |
//! | `cpp`            | `CMakeFiles`, `cmake-build-debug`, `cmake-build-release` |
//! | `android`        | `.cxx`, `externalNativeBuild` |
//! | `ruby`           | `.bundle` |
//! | `elixir`         | `_build`, `deps`, `cover` |
//! | `haskell`        | `dist-newstyle`, `.stack-work` |
//! | `scala`          | `.bloop`, `.metals`, `target` |
//! | `unity`          | `Library`, `Temp`, `Obj` |
//! | `unreal`         | `Intermediate`, `DerivedDataCache`, `Binaries` |
//! | `godot`          | `.import`, `.godot` |
//! | `data-science`   | `.ipynb_checkpoints`, `.dvc`, `.mlruns`, `outputs`, … |
//! | `infra`          | `.serverless`, `.vercel`, `.netlify`, `.terraform`, `.sass-cache`, … |
//! | `all`            | union of every profile above |
//!
//! ## Interactive TUI
//!
//! | Key                     | Action |
//! |-------------------------|--------|
//! | `↑` / `k` · `↓` / `j`   | move cursor |
//! | `d` / `Space` / `Enter` | open delete confirm |
//! | `y` / `n` / `Esc`       | confirm / cancel modal |
//! | `s`                     | toggle sort by **size** (desc default) |
//! | `n`                     | toggle sort by **name** (asc default) |
//! | `m`                     | toggle sort by **last-used** (desc default) |
//! | `r` / `F5`              | rescan |
//! | `q` / `Ctrl-C`          | quit |
//!
//! The header surfaces live progress (dirs scanned, releasable space,
//! reclaimed space) and the modal shows risk reasons for paths that
//! shouldn't normally be deleted (`~/.config`, AppData, `.app` bundles, …).
//!
//! ## Safety
//!
//! Two layered guards run **before** any filesystem mutation:
//!
//! 1. **Basename guard** — the path's basename must appear in the resolved
//!    target list. Catches "I typed the wrong path" mistakes.
//! 2. **Containment guard** — both the scan root and the target path are
//!    canonicalised (symlinks resolved). The canonical target must
//!    `starts_with` the canonical root. Catches symlink-escape attacks
//!    even if the link is named like a legitimate target.
//!
//! Underneath, `std::fs::remove_dir_all` is hardened against symlink
//! traversal ([CVE-2022-21658](https://blog.rust-lang.org/2022/01/20/cve-2022-21658.html)).
//!
//! ## Library usage
//!
//! cruftkill is also a library — the `cft` binary is just a TUI on top.
//! Import the [`core`] module to embed scanning, sizing, risk analysis,
//! and safe deletion in your own tool.
//!
//! ```no_run
//! use cruftkill::core::{scanner, types::ScanOptions};
//! use cruftkill::core::profiles::resolve_targets;
//!
//! # async fn run() -> anyhow::Result<()> {
//! let targets = resolve_targets(&["node", "rust"]);
//! let opts = ScanOptions {
//!     targets,
//!     exclude: vec![],
//!     sort_by: None,
//!     perform_risk_analysis: true,
//! };
//! let mut handle = scanner::start_scan("/home/me/Projects".into(), opts);
//!
//! while let Some(found) = handle.results.recv().await {
//!     println!("found: {} (sensitive: {:?})",
//!         found.path.display(),
//!         found.risk_analysis.as_ref().map(|r| r.is_sensitive));
//! }
//! # Ok(()) }
//! ```
//!
//! ## Architecture
//!
//! ```text
//! cft binary (src/main.rs)
//!    │
//!    ├── TUI mode  ──►  src/tui/   (ratatui + crossterm + tokio::select!)
//!    │                     │
//!    └── --no-tui  ──►  run_no_tui → NDJSON
//!                          │
//!                          ▼
//!                   src/core/
//!                      ├── scanner       parallel tokio worker pool
//!                      ├── size          refcounted async sum, 60s timeout
//!                      ├── risk          pure path classifier
//!                      ├── safe_delete   basename guard
//!                      ├── delete        canonicalize + remove_dir_all
//!                      ├── profiles      17 ecosystem profiles
//!                      ├── sort          path / size / age comparators
//!                      ├── filter        case-insensitive substring
//!                      ├── ignore        GLOBAL_IGNORE set (skip-on-walk)
//!                      ├── types         ScanOptions, FolderResult, …
//!                      └── error         CruftError (thiserror)
//! ```
//!
//! ## Links
//!
//! - GitHub: <https://github.com/xuanphamdev/cruftkill>
//! - crates.io: <https://crates.io/crates/cruftkill>
//! - Upstream inspiration: <https://github.com/voidcosmos/npkill>
//!
//! ## License
//!
//! MIT.

pub mod cli;
pub mod core;
pub mod tui;

pub use crate::core::error::CruftError;
pub use crate::core::types::{
    DeleteResult, FolderResult, RiskAnalysis, ScanFoundFolder, ScanOptions, SortBy, SortDirection,
};
