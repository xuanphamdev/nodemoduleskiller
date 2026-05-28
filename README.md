# cruftkill (`cft`)

[![Crates.io](https://img.shields.io/crates/v/cruftkill.svg)](https://crates.io/crates/cruftkill)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)](Cargo.toml)

```
 ██████╗███████╗████████╗
██╔════╝██╔════╝╚══██╔══╝
██║     █████╗     ██║
██║     ██╔══╝     ██║
╚██████╗██║        ██║
 ╚═════╝╚═╝        ╚═╝
```

**Polyglot dev-cache reaper.** Find and delete `node_modules`, `.venv`,
`target`, `DerivedData`, `__pycache__`, `obj`, `.gradle`, `.next`,
`.turbo` and the rest of your build cruft from a fast terminal UI.

Inspired by [voidcosmos/npkill](https://github.com/voidcosmos/npkill) — but
rewritten in Rust with a parallel async scanner and extended to 17 ecosystems.

> ⚠️ **`cft` deletes recursively without a recycle bin.** Always review the
> list before pressing `d`. Run with `--dry-run` first if you want to preview.

## Features (v0.2)

- 🌐 **17 hardcoded ecosystem profiles** — node, python, rust, java, swift,
  dotnet, ruby, elixir, haskell, scala, cpp, unity, unreal, godot, infra,
  data-science (or pass `--profile all`)
- 🔍 Parallel async directory scanner with `CancellationToken`
- 📏 True on-disk size per folder (Unix `blocks × 512`)
- 🛡️ Risk analyzer — flags paths inside `~/.config`, AppData, `.app` bundles
- 🗑️ Safe delete with two-layer guard (basename + canonicalize containment)
- 🖥️ Two UX modes:
  - **Interactive TUI** (ratatui): navigate, sort by size/name/last-used,
    delete with confirm, rescan
  - **`--no-tui` mode**: streams NDJSON for scripting / CI pipelines

## Install

From crates.io:

```bash
cargo install cruftkill
cft --help
```

From source:

```bash
git clone https://github.com/xuanphamdev/cruftkill
cd cruftkill
cargo install --path .
```

Requires Rust 1.85+ (edition 2024).

## Usage

### Interactive TUI

```bash
cft                            # scan current dir with `node` profile
cft ~/Projects                 # scan a specific directory
cft -p rust ~/code             # use the `rust` profile (matches `target/`)
cft -p node -p python ~/code   # combine profiles
cft -p all ~/                  # everything everywhere
cft --dry-run ~/Projects       # preview what would be deleted
```

Keybinds:

| Key | Action |
|---|---|
| `↑` / `k` | move cursor up |
| `↓` / `j` | move cursor down |
| `d` / `Space` / `Enter` | open delete confirm |
| `y` / `Y` | confirm delete |
| `n` / `N` / `Esc` | cancel modal |
| `s` | toggle sort by size (desc default) |
| `n` | toggle sort by name (asc default) |
| `m` | toggle sort by last-used (desc default) |
| `r` / `F5` | rescan |
| `q`, `Ctrl-C` | quit |

### Scriptable JSON output

```bash
cft --no-tui ~/Projects | jq '. | select(.size_bytes > 100000000)'
```

Each line is a JSON object:

```json
{
  "path": "/home/me/proj-a/node_modules",
  "size_bytes": 314572800,
  "is_sensitive": false,
  "risk_reason": null,
  "modified_unix": 1779867789,
  "dry_run": false
}
```

When stdout is not a TTY (e.g. piped or in CI), `cft` auto-falls-back to
`--no-tui` mode.

### Profiles

Run `cft --help` or read [`src/core/profiles.rs`](src/core/profiles.rs) for
the full target list. Highlights:

| Profile | Matches |
|---|---|
| `node` (default) | `node_modules`, `.npm`, `.pnpm-store`, `.next`, `.nuxt`, `.turbo`, `.cache`, `coverage`, … |
| `python` | `__pycache__`, `.pytest_cache`, `.venv`, `.tox`, `.mypy_cache`, … |
| `rust` | `target` |
| `java` | `target`, `.gradle`, `out` |
| `swift` | `DerivedData`, `.swiftpm` |
| `dotnet` | `obj`, `TestResults`, `.vs` |
| `cpp` | `CMakeFiles`, `cmake-build-debug`, `cmake-build-release` |
| `unity` | `Library`, `Temp`, `Obj` |
| `unreal` | `Intermediate`, `DerivedDataCache`, `Binaries` |
| `godot` | `.import`, `.godot` |
| `data-science` | `.ipynb_checkpoints`, `.dvc`, `.mlruns`, … |
| `infra` | `.serverless`, `.vercel`, `.netlify`, `.terraform`, … |
| `all` | union of every profile |

Add ad-hoc targets with `-t`:

```bash
cft -p node -t my_custom_cache ~/code
```

## Safety model

Two layered guards run before any FS mutation:

1. **Basename guard** — the path's basename must appear in the resolved
   target list. Catches the "wrong path" mistake.
2. **Containment guard** — both the scan root and the target path are
   canonicalized (symlinks resolved). The canonical target must
   `starts_with` the canonical root. Catches symlink escape attacks even
   if the link is named like a target.

`std::fs::remove_dir_all` is hardened against symlink-traversal
([CVE-2022-21658](https://blog.rust-lang.org/2022/01/20/cve-2022-21658.html))
— Rust does not follow symlinks when removing a directory.

## Architecture

```
cft binary (src/main.rs)
   │
   ├── TUI mode  ──►  src/tui/   (ratatui + crossterm + tokio::select!)
   │                     │
   └── --no-tui  ──►  src/main.rs::run_no_tui → NDJSON
                         │
                         ▼
                  src/core/
                     ├── scanner    (parallel tokio worker pool, unbounded dispatch)
                     ├── size       (refcounted async sum, 60s timeout)
                     ├── risk       (pure path classifier)
                     ├── safe_delete  (basename guard)
                     ├── delete     (canonicalize + remove_dir_all)
                     ├── profiles   (17 profiles, resolve_targets)
                     ├── sort       (path / size / age comparators)
                     ├── filter     (case-insensitive substring)
                     ├── ignore     (GLOBAL_IGNORE set)
                     ├── types      (ScanOptions, FolderResult, …)
                     └── error      (CruftError)
```

## Development

```bash
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo build --release
```

## History

This crate was originally published as
[`nodemoduleskiller`](https://crates.io/crates/nodemoduleskiller) v0.1.0
(binary `nmk`). It was renamed to **`cruftkill`** (binary `cft`) in v0.2.0
because the scope had outgrown "node_modules only" — the tool now wipes 17
different language ecosystems' build cruft from a single command. The
GitHub repository was renamed too; the old URL `xuanphamdev/nodemoduleskiller`
auto-redirects to `xuanphamdev/cruftkill`.

## Attribution

This project was inspired by — and ported from —
[voidcosmos/npkill](https://github.com/voidcosmos/npkill) (© voidcosmos, MIT
license). Many design decisions (target detection rules, risk-analysis
heuristics, profile definitions, behavioural invariants) are preserved
verbatim.

## License

MIT — see [`LICENSE`](LICENSE).
