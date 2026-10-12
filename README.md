# Glyphst

Glyphst is a workspace for desktop applications built with Rust,
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), and
[GPUI Kit](https://github.com/longbridge/gpui-kit) (through its `gpui-component` facade).

## Applications

### Glyphst Reader

A PDF and EPUB reader with tabbed reading, search, document navigation, annotations, AI chat,
translation workflows, themes, and configurable keybindings.

### Glyphst Files

A lightweight, read-only file navigator with Yazi-inspired parent, current-directory, and preview
panes. It supports Vim-style keyboard navigation and line-oriented UTF-8 text previews.

## Documentation

- [Architecture](ARCHITECTURE.md): ownership, persistence, GPU lifetimes, and dependencies.
- [Next work](ROADMAP.md): prioritized follow-ups and completion criteria.
- [Agent guidance](AGENTS.md): repository constraints and task-specific verification.

## Development

The Rust version in [rust-toolchain.toml](rust-toolchain.toml) and GPUI's native libraries
are needed. Glyphst Reader additionally requires MuPDF. On Linux, the Nix
development shell provides the required libraries and tools.

```sh
nix develop
cargo run -p glyphst-reader --release -- /path/to/document.pdf
cargo run -p glyphst-reader-egui --release -- /path/to/document.pdf   # the egui Reader (in progress)
cargo run -p glyphst-files -- /path/to/start
```

Without Nix, install the platform packages listed in
[CI](.github/workflows/ci.yml) before running Cargo.

Normal builds use the published Git revisions in `Cargo.toml` and `Cargo.lock`;
no dependency submodule or sibling checkout is required. Nix uses the same Rust
toolchain file and Cargo lockfile. The read-only repositories in `references/`
are optional research material, not build inputs.

### Working on maintained dependencies

GPUI comes from the crates.io `gpui-pre` snapshots of Zed (`gpui-pre`,
`gpui-pre-platform`, `gpui-pre-ztracing`), pinned to the exact version GPUI Kit
uses. The only maintained fork is GPUI Kit: keep its checkout beside this
repository at `../gpui-kit` on `glyphst/main`. `references/zed` is a read-only
upstream reference.

For local development, put only the overrides you need in the ignored
`.cargo/config.local.toml`:

```toml
[patch."https://github.com/glyphst/gpui-kit"]
gpui-component = { path = "../gpui-kit/crates/component" }
gpui-kit-assets = { path = "../gpui-kit/crates/assets" }
gpui-fps = { path = "../gpui-kit/crates/fps" }
gpui-base = { path = "../gpui-kit/crates/base" }
```

Use `cargo --config .cargo/config.local.toml check --workspace --all-targets --all-features`.
These overrides are opt-in; normal Cargo commands and CI do not load this file.
Do not commit override-generated lockfile changes; `cargo fmt` and other
commands that run without the config resolve the published pins again. Publish
GPUI Kit before updating this workspace's revisions. Omit the local config, run
`cargo metadata --format-version 1` to restore Git resolution, and review the
lockfile before the normal `--locked` checks.

See [architecture and dependency boundaries](ARCHITECTURE.md) for ownership,
persistence, cache limits, and the remaining dependency-audit exception.

To inspect the file-open animation, exit any running Reader and start a debug build with a minimum
loader duration in milliseconds:

```sh
GLYPHST_DEBUG_FILE_OPEN_MIN_MS=3000 cargo run -p glyphst-reader -- /path/to/document.pdf
```

Release builds ignore this variable.

### Verification

`script/check.sh` runs the tiered checks inside the Nix shell and records step
logs and timings under `target/check-reports/`:

```sh
script/check.sh quick          # fmt, clippy, tests
script/check.sh gate           # + doctests, cargo audit, release Reader build
script/check.sh gate --local   # same, against sibling checkouts (.cargo/config.local.toml)
script/check.sh corpus         # GPU page rendering against MuPDF on sample PDFs
```

The `corpus` tier renders page 1 of up to `GLYPHST_PDF_CORPUS_LIMIT` (100) PDFs
from `GLYPHST_PDF_CORPUS` (default: the pdf.js test corpus under `../bak`) and
fails only on crashes or timeouts after parsing. For zoom timing on one page:

```sh
cargo run --release -p glyphst-pdf-gpu --example zoom_bench -- page.pdf --out target/check-reports/zoom-bench
cargo run --release -p glyphst-pdf-gpu --example zoom_view -- page.pdf   # interactive
```

The egui Reader runs the same checks unattended in its own window, saving
nothing: `--bench` prints zoom, scroll and whole-document search frame
times as JSON, and `--screenshot PATH`, `--screenshot-turn PATH` and
`--screenshot-switch PATH` (two documents) capture the window, the latter two
halfway through a transition. `--zoom Z`, `--page N`, `--background ID` and
`--transition ID|none` set up the run; `--find TEXT`, `--select-page N`,
`--sidebar TAB` and `--font-size PT` (EPUB) show a search, a selection, a
sidebar tab or a reflowed size in the screenshot. `--translate-fake`
translates through a local fake provider (no keys, no network), and
`--translate-document` / `--translate-selection` start a translation that a
screenshot waits for and a bench runs alongside. `--ai-fake` answers the
assistant from a local fake model, and `--ask TEXT` asks it at startup.

Report baseline failures separately. The rendering debug panel's frame capture
measures interactive performance manually. Review [audit exceptions](ARCHITECTURE.md#dependency-audit)
when dependencies change. Documentation-only changes need content/link checks
and `git diff --check`.

Install the configured pre-commit hooks with `prek install`.

## Reader Profiling

Start Tracy and run:

```sh
ZTRACING=1 cargo run -p glyphst-reader --features tracy --release
```

## License

Glyphst is licensed under [AGPL-3.0-only](LICENSE).
