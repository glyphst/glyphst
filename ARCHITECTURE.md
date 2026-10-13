# Architecture and dependency boundaries

## Ownership

| Owner | Responsibility |
| --- | --- |
| `reader`, `files` | Application entry points |
| `workspace` | Panes and tabs (on the GPUI Kit dock), side docks, home/library browsing, window commands |
| `document-ui` | Document views, interaction, and UI task lifetimes |
| `ui`, `actions`, `keymap` | Shared widgets and application input |
| `settings`, `settings-ui` | Settings data and its editor |
| `library` | Managed documents, annotations, reader sessions, persistence |
| `ai`, `ai-ui` | AI services and chat presentation |
| Translation crates | Translation domain, PDF production, service orchestration, UI |
| `pdf` | MuPDF integration: document opening, render sessions, display lists, vector capture |
| `pdf-gpu` | GPU vector page rendering from captured pages (wgpu 30) |
| `effects` | Shader effects (transitions, backgrounds, overlays) defined as data; wgpu only |
| `reader-egui` | The egui Reader (in progress, alongside the GPUI `reader`): tabs, home, GPU document view, text selection, find, links, annotations, EPUB reflow, translation, assistant, commands and settings |

Private modules split implementation without introducing additional state
owners. New-tab and workspace entities own their task handles; document views
own their render requests, text indexing, and cancellation. See
[workspace](crates/workspace/src) and [document views](crates/document-ui/src/document_view).

MuPDF context clones share stylesheet settings across threads.
[Document opening](crates/pdf/src/document.rs) serializes configuration. PDF
opens preserve the user CSS allocation and enable document styles for rich-text
appearances, avoiding repeated writes when already enabled. EPUB stylesheet
and publisher-style settings still have process-wide ownership; concurrent
documents cannot rely on independent EPUB style policies.

## Presentation and task lifetime

PDF and EPUB pages are rasterized by MuPDF off the UI thread and presented as
`RenderImage`s. Render keys identify the document, page, source revision,
rotation, color transform, and physical size. The process-wide render cache
bounds raster bytes and display lists; keys for the current viewport are
protected from eviction while they are displayed or pending. Visible pages are
requested first and adjacent pages are prefetched within a pixel limit. Until
the exact raster is ready, the closest cached raster for the page remains
visible.

## GPU vector rendering

[Vector capture](crates/pdf/src/vector.rs) replays a MuPDF display list
through a native device and records fills, strokes and glyph outlines in page
space. Rectangular clips become scissors and other path clips stencil clips.
Shadings, images, soft masks, non-normal blend groups, patterns and text or
stroke clips stay ranges of display-list operations that MuPDF rasterizes as
ordered layers, so no page falls back as a whole. [`pdf-gpu`](crates/pdf-gpu)
tessellates each page once per level of detail and draws it with a camera
uniform; dense pages also get a mipmapped overview rendered from their own
vectors and drawn while it is at least as sharp as the screen. The Reader
does not use it; GPUI 0.3.8 cannot composite a wgpu texture on Linux. The
[egui Reader](crates/reader-egui) does (see [next work](ROADMAP.md)).
`pdf-gpu` uses wgpu 30, egui's version; GPUI keeps its own wgpu 29.

Images and image masks become textures at their own resolution. In the egui
Reader, an `egui_wgpu` paint callback draws a view in three steps (following
photocraft's `gpu_canvas`): `prepare` renders paper and resident pages into a
multisampled `OffscreenTarget`; a background effect fills the view; a
content effect composites the pages, or a transition from a snapshot of the
previous view (another tab, the page before a turn). Raster layers are
re-rasterized for the view once it is still. Each page is scissored to its
page box, as MuPDF clips to its pixmap. Pages load on a worker thread per
document (which also builds image mip chains) and stay within a GPU byte
budget. A loaded page, and a raster layer re-rasterized for the view, is
written to the GPU at most 4 MiB per frame through one reused
`StagingBelt` and drawn once complete; overviews are built one tile per
frame. The device uses 8-32 MiB memory blocks instead of wgpu's 128-256 MiB:
the first submission into a new block waits while the driver prepares it.

## egui Reader boundaries

The egui Reader follows the patterns of artcraft's egui apps (pdfcraft's
home, pickers, last session and theme; craft-ui tabs; filmcraft's
data-defined effects) rather than reproducing the GPUI Reader. It reuses
`pdf`, `pdf-gpu`, `effects`, `library` and the translation domain crates;
`library`, `settings` and `ai` build without GPUI, and their `gpui` features
add what the GPUI crates need (the app globals, the settings store with its
credential worker, gpui-component themes, ChatGPT Subscription sign-in).
The egui Reader shares the GPUI Reader's settings file and, with the
`settings` crate's `keychain` feature, the provider keys it keeps in the
system credential store (items written as GPUI writes them). Its settings
window edits only the provider sections (AI, translation, OCR): a save
re-reads the file and replaces just those, keys the file held stay in it and
other keys go to the keychain. A GPUI Reader running at the same time keeps
its own copy and overwrites those sections on its next save. Scripted runs
and fake providers keep settings and keys in memory. Both readers
share the redb library, which one process holds at a time: while the GPUI
Reader runs, the egui Reader opens documents without history. Its own
preferences (theme, transition, background, keyboard shortcuts, last
session) live in eframe
storage under the config directory; scripted runs (`--bench`,
`--screenshot…`) save nothing and leave the library unwritten. User effects
are WGSL files with `// @effect` manifests in `<config>/glyphst/effects`.

Its components take pdfcraft's style: the design tokens (`theme.rs`, light
and dark, white pages in both), Inter and JetBrains Mono embedded ahead of
egui's faces and a system CJK fallback, Lucide SVG icons through craft-ui's
icon set and buttons (`icons.rs`), and pdfcraft's widgets (`widgets.rs`:
pills, search box, section titles, segmented choices, cards, toasts). The
fonts and icons ship in `crates/reader-egui/assets` with their licences.
The layout is its own: below the top bar (tabs, the palette's search box,
theme, settings) a document's stage draws in two GPU callbacks, the
background effect over the whole stage and then the pages over a reading
rectangle, with egui page shadows between them. The rail, the drawers, the
dock and the progress line are egui Areas above the stage; `stage.rs` lays
them out (drawers push the pages aside, or float over them in a narrow
window) and fades the dock and rail after a pause in pointer movement.

Every user action is a command in a registry (after pdfcraft's): an id,
a label, default keys and when it can run. Keys, the command palette
(Ctrl+K), menus and buttons all run commands through one dispatcher. The
user's shortcuts replace a command's default keys and are kept by command
id in the egui Reader's preferences; the GPUI Reader's keymap is separate.

Translation runs the domain services on worker threads: a selection becomes a
card, and a PDF becomes a translation session whose pages (one-page PDFs
typeset by Typst, in a worker process of the reader's executable) are drawn
in place of the originals as they arrive. Each page has a source version
(original, or a preview revision); the loader and indexer load and read the
version shown, and a page's GPU drawing is replaced only when the new version
is ready, so the original stays visible meanwhile. Selection, copy and search
use the translated text; hovering a translated block shows the original.

The assistant runs the `ai` crate's agent on a worker thread with the main
model from the settings, the current page and selection as optional context,
and document tools (`search_document`, `read_document`,
`render_document_pages`) with the GPUI Reader's names and schemas,
implemented over MuPDF and `pdf::text`. Conversations are the library's AI
sessions, so both readers list them; image results are not stored.
ChatGPT Subscription sign-in remains in the GPUI Reader.

Page text comes from MuPDF structured text ([`pdf::text`](crates/pdf/src/text.rs))
with whitespace kept and ligatures expanded, as the GPUI Reader indexes it, so
glyph offsets in reflowed positions and highlight anchors resolve in both
readers. Text follows each loaded page; a second worker per document reads
the remaining pages' text for search and reflowed documents, and sidebar
thumbnails. Selections, search matches and highlights are egui shapes over
the GPU view. Positions store pages from 1, as the library does. Fixed-layout
highlights and notes keep page rectangles; reflowed ones keep a semantic range
and are found again after a relayout. EPUBs are laid out for the view's width
(capped by the document's reading preferences) and shown at actual size.

## Library persistence

[Library storage](crates/library/src/library/storage.rs) has one current-format
initialization path for production and tests. `open_for_test` supplies a test
root; it does not select another schema or run legacy migrations. Unsupported
schema metadata is rejected without modification. Table names, serialization
keys, schema metadata, managed-library layout, and current-format decode
fallbacks are compatibility contracts.

Glyphst carries no database migrations; there are no external users to
preserve data for. When redb reports a file from an older file format
(`UpgradeRequired`), the library and the translation cache rename it aside as
`<name>.redb<version>-format` (never deleting it) and start a fresh database.

## Dependency maintenance

[Cargo.toml](Cargo.toml) and [Cargo.lock](Cargo.lock) pin GPUI to an exact
crates.io `gpui-pre` snapshot and GPUI Kit to a published fork revision. Cargo and Nix use [rust-toolchain.toml](rust-toolchain.toml).
Sibling checkouts are optional; local overrides are described in
[README](README.md#working-on-maintained-dependencies).

For the maintained GPUI Kit fork, `main` is a fast-forward-only mirror of
`upstream/main`; `glyphst/main` is upstream plus a short, explained delta listed
in the fork's `AGENTS.md`. Prefer GPUI Kit components and upstream APIs; keep
composable widgets, styling workarounds, and application keybindings here, and
patch Kit only when it lacks a capability Glyphst needs. Preserve the
`gpui_component`, `gpui_component_assets`, `gpui_base` and `gpui_fps` aliases;
`gpui_component_assets` resolves to `gpui-kit-assets`.

The app's `gpui-pre` pin must equal the snapshot GPUI Kit requires; bump both
together. Publish Kit, then update Glyphst's pins. Exact renderer/compiler/math pins and major library
upgrades require compatibility work. Do not commit override-generated lockfile
changes.

`references/` contains shallow upstream reference submodules, including Zed;
these are not maintained forks or build inputs. To refresh an intended reference,
use `git submodule update --remote --depth 1 references/<name>` and review its
gitlink. Preserve copied-source and theme provenance independently of renames.

## Dependency audit

[The audit policy](.cargo/audit.toml) records the reviewed quick-xml exceptions
RUSTSEC-2026-0194 and RUSTSEC-2026-0195. At the October 11, 2026 checkpoint,
`quick-xml 0.38.4` remains through `citationberg 0.7.0 -> hayagriva 0.10.1 ->
typst-library 0.15.1`; Typst 0.15.1 is still the latest release. This is runtime CSL parsing. The Typst file loader limits
project/package access but does not generally bound XML CPU or allocation work.

A configured audit pass does not mean the dependency graph is warning-free.
Exception removal and remaining warning paths are tracked in
[ROADMAP](ROADMAP.md#3-dependency-audit-follow-up); refresh the advisory database
before treating old results as current security evidence.
