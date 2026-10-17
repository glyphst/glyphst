# Glyphst

Glyphst Reader is a PDF and EPUB reader built with Rust,
[egui](https://github.com/emilk/egui) and [wgpu](https://wgpu.rs): pages are
drawn from their vectors on the GPU, so zooming stays sharp and smooth.

## Glyphst Reader

Tabs and split panes, a library home, search, links and history,
bookmarks, highlights and notes, two-page spreads and rotation, document and
selection translation, an AI assistant, shader effects for page turns and
backgrounds, a command palette and configurable shortcuts with a Vim
keymap (the Keyboard shortcuts tab, or "Keys: Vim" in the palette), and an
optionally transparent window (Settings › Appearance).

## Documentation

- [Architecture](ARCHITECTURE.md): ownership, persistence, GPU lifetimes, and dependencies.
- [Next work](ROADMAP.md): prioritized follow-ups and completion criteria.
- [Agent guidance](AGENTS.md): repository constraints and task-specific verification.

## Development

The Rust version in [rust-toolchain.toml](rust-toolchain.toml), MuPDF's build
tools and the windowing and Vulkan libraries are needed. On Linux, the Nix
development shell provides them.

```sh
nix develop
cargo run -p glyphst-reader --release -- /path/to/document.pdf
```

Without Nix, install the platform packages listed in
[CI](.github/workflows/ci.yml) before running Cargo.

Normal builds use the published Git revisions in `Cargo.toml` and `Cargo.lock`;
no dependency submodule or sibling checkout is required. Nix uses the same Rust
toolchain file and Cargo lockfile. The read-only clones in the workspace's
`../references/` are optional research material, not build inputs.

See [architecture and dependency boundaries](ARCHITECTURE.md) for ownership,
persistence, cache limits, and the remaining dependency-audit exception.

### Verification

`script/check.sh` runs the tiered checks inside the Nix shell and records step
logs and timings under `target/check-reports/`:

```sh
script/check.sh quick          # fmt, clippy, tests
script/check.sh gate           # + doctests, cargo audit, release Reader build
script/check.sh corpus         # GPU page rendering against MuPDF on sample PDFs
script/check.sh screens        # every design frame in both themes, as PNGs
```

The `corpus` tier renders page 1 of up to `GLYPHST_PDF_CORPUS_LIMIT` (100) PDFs
from `GLYPHST_PDF_CORPUS` (default: the pdf.js test corpus under `../bak`),
and 20 of them turned 90 degrees, and fails only on crashes or timeouts after
parsing. For zoom timing on one page:

```sh
cargo run --release -p glyphst-pdf-gpu --example zoom_bench -- page.pdf --out target/check-reports/zoom-bench
cargo run --release -p glyphst-pdf-gpu --example zoom_view -- page.pdf   # interactive
```

The Reader runs the same checks unattended in its own window, saving
nothing: `--bench` prints zoom, scroll and whole-document search frame
times as JSON, and `--screenshot PATH`, `--screenshot-turn PATH` and
`--screenshot-switch PATH` (two documents) capture the window, the latter two
halfway through a transition; `--screenshot-generating PATH` and
`--screenshot-revealing PATH` replay the loading animation over the drawn
pages and capture it while the page's parts are traced, and halfway
through the reveal; `--screenshot-present-tools PATH` (with `--command
view.present`) sets out presenting's ink, toolbox and tool ring first.
`--zoom Z`, `--page N`, `--background ID` and
`--transition ID|none` set up the run, `--theme system|light|dark` its
colours, `--gallery` shows every component of the design instead of home,
`--split right|down` shows the second document beside the
first, `--rotate 90|180|270` and `--spread` turn and spread the pages
(without remembering it), and `--overlay ID|none` and `--wallpaper PATH`
set both themes' page tint and the wallpaper; `--colours preset|background|custom`
picks where the theme's colours come from, and `--opacity O` makes the
window transparent at that opacity; `--find TEXT`, `--select-page N`
(or `--select-pages A-B`), `--sidebar TAB` and `--font-size PT` (EPUB)
show a search, a selection, a
sidebar tab or a reflowed size in the screenshot. `--translate-fake`
translates through a local fake provider (no keys, no network), and
`--translate-document` / `--translate-selection` start a translation that a
screenshot waits for and a bench runs alongside. `--ai-fake` answers the
assistant from a local fake model, and `--ask TEXT` asks it at startup.
`--command ID` (repeatable) runs a command by its id, such as `view.zen`,
once the documents are open, and `--select-region PAGE:X0,Y0,X1,Y1` draws a
region (in page points) as the region tool would.
`--palette QUERY` opens the command palette and `--settings SECTION` the
settings window (`appearance`, `reading`, `effects`, `translation`, `ocr`,
`ai`; `appearance:background` and `appearance:colours` scroll to that
group), or with `shortcuts` the Keyboard shortcuts tab. Scripted runs keep
settings and keys in memory.

### Writing an effect

Settings › Effects › New effect writes a commented starter
(`crates/effects/src/template.wgsl`) into `<config>/glyphst/effects` and
opens the folder; the Reader reloads effects when a file there changes, and
lists the ones that fail to compile on that page. An effect is a WGSL
`fn effect(uv, pixel) -> vec4<f32>` returning premultiplied colour after
the shared [prelude](crates/effects/src/prelude.wgsl), with `// @effect`
lines naming it, its kind (`background`, `transition` or `overlay`) and up
to four parameters for the sliders.

Report baseline failures separately. The statistics overlay (F12) shows
frame times and GPU memory while reading. Review
[audit exceptions](ARCHITECTURE.md#dependency-audit) when dependencies
change. Documentation-only changes need content/link checks and
`git diff --check`.

Install the configured pre-commit hooks with `prek install`.

## License

Glyphst is licensed under [AGPL-3.0-only](LICENSE).
