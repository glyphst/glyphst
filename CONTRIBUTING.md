# Contributing to Glyphst

Glyphst is written in Rust, with [egui](https://github.com/emilk/egui) and
[wgpu](https://wgpu.rs) for the interface and the GPU, and
[MuPDF](https://mupdf.com) for parsing documents.

- [Architecture](ARCHITECTURE.md): ownership, persistence, GPU lifetimes and
  dependencies.
- [Roadmap](ROADMAP.md): prioritized follow-ups and completion criteria.
- [Agent guidance](AGENTS.md): repository constraints and the checks each
  kind of change needs.

## Building

The Rust version in [rust-toolchain.toml](rust-toolchain.toml), MuPDF's build
tools and the windowing and Vulkan libraries are needed. On Linux, the Nix
development shell provides them.

```sh
git clone https://github.com/glyphst/glyphst
cd glyphst
nix develop
cargo run -p glyphst-reader --release -- /path/to/document.pdf
```

The CJK faces (Noto Sans CJK and Source Han Serif, a pack for each of Chinese
(Simplified), Chinese (Traditional), Japanese and Korean) are read at run time
from `$GLYPHST_FONTS_DIR`, the packs Settings · Resources downloads, or a
`../fonts` folder laid out the same way (`fonts/manifest.txt`,
`fonts/noto-sans-cjk-sc/…`), which `script/check.sh screens` needs so every
shot has the same faces. Without them, CJK text in the interface falls back to
an installed font, and a translation into Chinese, Japanese or Korean uses the
computer's CJK fonts or offers the download.

`cargo build --release` is the quick local build. Shipped builds use
`script/bundle <target>`: the `dist` profile (thin LTO, one codegen unit,
stripped, `opt-level = "z"` for Glyphst's cold crates) with the linker flags in
`.cargo/dist.toml`, packaged for the target's system; `script/check.sh size`
measures the executable.

The app icon is frame 11 of `design/reader/main.pen`. Its two masters are
Typst files in [crates/reader/resources/app-icon](crates/reader/resources/app-icon);
after changing them, `script/icons` renders the PNGs, SVGs and `.ico` there
(commit them). The macOS `Info.plist` and the Windows installer script are
beside them in `crates/reader/resources/`.

Without Nix, install the platform packages listed in
[CI](.github/workflows/ci.yml) before running Cargo.

Normal builds use the published Git revisions in `Cargo.toml` and `Cargo.lock`;
no dependency submodule or sibling checkout is required to build. Nix uses the same Rust
toolchain file and Cargo lockfile. The read-only clones in the workspace's
`../references/` are optional research material, not build inputs.

See [architecture and dependency boundaries](ARCHITECTURE.md) for ownership,
persistence, cache limits, and the remaining dependency-audit exception.

## Releasing

Versions are dates: `YY.M.D`, such as `26.10.25` for 25 October 2026. A
second release on the same day adds a number: Cargo version `26.10.25+1`,
released as `26.10.25.1`.

Set `[workspace.package] version` in `Cargo.toml`, commit, and push the tag
`v<version>` (`v26.10.25`, or `v26.10.25.1`). The
[release workflow](.github/workflows/release.yml) builds the Reader for
Linux (glibc 2.31 or newer) and Windows, each on x86_64 and aarch64, and macOS
11 or newer on Apple silicon (Intel Macs are not supported), packages it with
`script/bundle`, and publishes the packages with `SHA256SUMS` as the GitHub
Release of that tag (a prerelease when the version has a `-`):

| System | Package |
| --- | --- |
| Linux | `glyphst-reader-<version>-linux-<arch>.tar.gz`: `bin/`, the desktop entries and the icons, laid out to extract into a prefix such as `~/.local` |
| macOS | `glyphst-reader-<version>-macos-aarch64.dmg`: `Glyphst Reader.app` and a link to Applications |
| Windows | `glyphst-reader-<version>-windows-<arch>-setup.exe`: an Inno Setup installer for the current user; and `….zip`, the executable alone |

Running the workflow by hand builds them without publishing. The macOS app
is signed ad hoc only (no Developer ID, not notarized), and the Windows
installer and executable are not signed.

## Verification

`script/check.sh` runs the tiered checks inside the Nix shell and records step
logs and timings under `target/check-reports/`:

```sh
script/check.sh quick          # fmt, clippy, tests
script/check.sh gate           # + doctests, cargo audit, release Reader build
script/check.sh corpus         # GPU page rendering against MuPDF on sample PDFs
script/check.sh screens        # every design frame in both themes, as PNGs
script/check.sh screens 02d --theme dark   # only the named frames
```

Screens run in a private headless sway (`script/headless-session.sh`), four
Readers at a time, so no window reaches the desktop and they keep going while
the screen is locked.

The `corpus` tier renders page 1 of up to `GLYPHST_PDF_CORPUS_LIMIT` (100) PDFs
from `GLYPHST_PDF_CORPUS` (default: the pdf.js test corpus under `../bak`),
and 20 of them turned 90 degrees, and fails only on crashes or timeouts after
parsing. For zoom timing on one page:

```sh
cargo run --release -p glyphst-pdf-gpu --example zoom_bench -- page.pdf --out target/check-reports/zoom-bench
cargo run --release -p glyphst-pdf-gpu --example zoom_view -- page.pdf   # interactive
```

The Reader runs the same checks unattended in its own window, saving
nothing: `--bench` prints zoom, scroll, sidebar-slide and whole-document
search frame times as JSON, and `--screenshot PATH`, `--screenshot-turn
PATH` and `--screenshot-switch PATH` (two documents) capture the window, the
latter two halfway through a transition; `--screenshot-generating PATH` and
`--screenshot-revealing PATH` replay the loading animation over the drawn
pages and capture it while the page's parts are traced, and halfway
through the reveal; `--screenshot-present-tools PATH` (with `--command
view.present`) sets out presenting's ink, toolbox and tool ring first;
`--screenshot-slide PATH` captures the sidebar partway through sliding
in or out, the pages resizing with it. `--zoom Z`, `--page N`, `--background ID` and
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

## Writing an effect

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
