# Architecture and dependency boundaries

## Ownership

| Owner | Responsibility |
| --- | --- |
| `reader` | The Reader (egui): tabs and split panes, home, GPU document view, page overview, text and region selection, find, links, annotations, EPUB reflow, translation, assistant and quick ask, commands and settings |
| `ui` | The glyph design system on egui: tokens, type, icons and one component per component of `design/main.pen` |
| `settings` | Settings data, its KDL file and the keychain |
| `library` | Managed documents, annotations, reader sessions, persistence |
| `ai` | AI services: providers, the agent, ChatGPT Subscription sign-in |
| `agent-host` | External agents (Claude Code, Codex, Pi, ACP agents): their drivers, working folders with git checkpoints, and the MCP server that serves them the Reader's tools |
| Translation crates | Translation domain, PDF production, service orchestration |
| `pdf` | MuPDF integration: document opening, render sessions, display lists, vector capture |
| `pdf-gpu` | GPU vector page rendering from captured pages (wgpu 30) |
| `effects` | Shader effects (transitions, backgrounds, overlays) defined as data; wgpu only |
| `mermaid-render` | Mermaid diagrams to SVG (not used yet; for the assistant) |

Private modules split implementation without introducing additional state
owners. Each document tab owns its loader and indexer workers, their
requests and their cancellation (see [the document tab](crates/reader/src/document_tab.rs)).

MuPDF context clones share stylesheet settings across threads.
[Document opening](crates/pdf/src/document.rs) serializes configuration. PDF
opens preserve the user CSS allocation and enable document styles for rich-text
appearances, avoiding repeated writes when already enabled. EPUB stylesheet
and publisher-style settings still have process-wide ownership; concurrent
documents cannot rely on independent EPUB style policies.

## GPU vector rendering

[Vector capture](crates/pdf/src/vector.rs) runs MuPDF over a page (or
replays a display list) through a native device and records fills, strokes
and glyph outlines in page space. Rectangular clips become scissors and other
path clips stencil clips. Shadings, images, soft masks, non-normal blend
groups, patterns and text or stroke clips stay ranges of display-list
operations that MuPDF rasterizes as ordered layers, so no page falls back as
a whole. The Reader captures straight from MuPDF's interpreter, handing out
the draws captured so far every 250 ms once a page has taken 300 ms; their
one-level previews draw, in order, until the page itself is written. The
page's display list (thumbnail, text, re-rasterized layers) is made after
the page is sent. A page with raster layers is captured again from its list,
because a list's replay culls operations (paths without points, contents of
clips with no area) and the layers' ranges are numbered by the list. A
captured page that took at least a second, with no raster layers, is kept
in the [page cache](crates/pdf/src/vector_cache.rs) under the cache
directory's `pages/` (2 GiB, least recently used first out), one file per
document page, checked against the document's and the Reader's size and
modification time, so reopening it skips MuPDF. [`pdf-gpu`](crates/pdf-gpu)
prepares each page once per level of detail and draws it with a camera
uniform (pan, zoom and quarter turns): fills are quads whose pixels integrate
the path's coverage from its edges (HEPR's analytic fills, banded when long)
and strokes are capsule quads, drawn in paint order by one pipeline; fills
too dense for bands and clip paths are tessellated with lyon. Dense pages
also get a mipmapped overview rendered from their own
vectors and drawn while it is at least as sharp as the screen. `pdf-gpu`
uses wgpu 30, egui's version.

Images and image masks become textures at their own resolution. In the
[Reader](crates/reader), `egui_wgpu` paint callbacks draw a document (following
photocraft's `gpu_canvas`): `prepare` renders paper and resident pages into a
multisampled `OffscreenTarget`, and a content effect composites the pages,
or a transition from a snapshot of the previous view (another tab, the page
before a turn), over the background effect. Raster layers are
re-rasterized for the view once it is still. Each page is scissored to its
page box, as MuPDF clips to its pixmap. Pages load on a worker thread per
document (which also builds image mip chains) and stay within a GPU byte
budget. A loaded page, and a raster layer re-rasterized for the view, is
written to the GPU at most 4 MiB per frame through one reused
`StagingBelt`: the coarsest level, the images and the raster layers come
first, so a page draws coarse once they are written and sharpens as finer
levels arrive (an older version of it stays drawn until the new one is
complete); overviews are built one tile per frame. The device uses 8-32 MiB memory blocks instead of wgpu's 128-256 MiB:
the first submission into a new block waits while the driver prepares it.

## Reader boundaries

The Reader follows the patterns of artcraft's egui apps (pdfcraft's home,
pickers, last session and theme; craft-ui tabs and docking; filmcraft's
data-defined effects). It replaced a GPUI Reader in October 2026 and keeps
that Reader's data: the settings file, the provider keys in the system
credential store (items in the form GPUI wrote, through the `settings`
crate's `keychain` feature), and the redb library, which one process holds
at a time (a second Reader opens documents without history). Its settings
window edits only the provider sections (AI, translation, OCR) and the
window's transparency: a save re-reads the file and replaces just those, so
the file's other sections survive; keys the file held stay in it and other
keys go to the keychain.
Scripted runs and fake providers keep settings and keys in memory. Its own
preferences (theme and the rest of Settings › Appearance, transition,
background, keyboard shortcuts, last session with its panes, page counts
for home) live in eframe storage under the config directory
(`glyphst/reader-egui/app.ron`, the path from before the switch); scripted
runs (`--bench`, `--screenshot…`) save nothing and leave the library
unwritten.

Effects are WGSL files with `// @effect` manifests ([effects](crates/effects)).
The registry keeps the built-ins and the user's effects (from
`<config>/glyphst/effects`) in two layers: a user effect may replace a
built-in by id, and the user layer is reloaded whole when the folder's
files change (checked once a second while the window has focus). Three
kinds are chosen in the Reader: the background behind the whole stage
(the focused book's; `image: true` backgrounds draw the wallpaper, read
and mipmapped on a worker thread), the page transition (the incoming
book's) and the page tint, an overlay that each view draws from its pages
into a tinted image before the content effect, so tints survive
transitions and splits. Defaults are set in Settings (the background in
Appearance as a theme colour, a colour, the wallpaper with its dim, blur
and parallax, or an effect; a page tint per light and dark theme, also in
Appearance; transitions in Effects), with parameters per effect (a
`colour` parameter is sRGB packed in a float; `themed` tints get the
theme's text and pane colours from the app); a book's own choices (the Effects drawer) live in
the Reader's preferences by document path, not in the library, whose
formats are compatibility contracts. Preview cards are paint callbacks
drawing each effect over two sample pages (or the wallpaper).

The look is the glyph design in `design/main.pen` (monospace chrome, prose
in Inter, hairline panes, one accent, radius 0/2/4 and no shadows; see
[design/README.md](../design/README.md)). Its tokens, type, icons and
components live in [`ui`](crates/ui) (`glyphst-ui`), one module per pen
component (buttons, keys, chips, toggles, sliders, meters, fields,
segmented choices, tooltips and toasts, tabs, the rail, pane headers, the
status bar, list and tree rows, menus, covers, chat pieces, page
thumbnails, settings rows) plus the glyph motifs (rule titles, tree
branches, meter glyphs, corner ticks, dot fields). Components read
`Tokens::get` only; screens pick no colours, radii or fonts of their own.
`theme::apply` takes an `Appearance` (theme, palette, opacity, density, ui
scale, ornaments, fonts). The palette is an accent swatch or a scheme
generated (`scheme.rs`, with `material-colors`) from the background's
colour, the wallpaper's (found by the wallpaper worker) or a custom one:
Material You palettes drawn at each design token's tone, so contrast
stays the design's. `fonts::install` puts the chosen chrome and
prose families (bundled JetBrains Mono and Inter, or installed ones found
through fontconfig) ahead of egui's faces and a system CJK fallback; with
ornaments off the motifs draw plain labels. `--gallery` draws every
component (frame 01) for comparing with the design. The fonts and Lucide
icons ship in `crates/ui/assets` with their licences. The System theme
follows the desktop: winit reports no theme on Linux, so
`desktop_theme.rs` polls the XDG portal's `color-scheme` (after pdfcraft);
without an answer System is light.
The window is always created transparent-capable and drawn opaque until
transparency is on (off by default). Then the base layer and the docked
panes take the window's opacity; everything raised over them (popovers,
menus, dialogs, fields) and the pages stay opaque. Blur behind the window
is the compositor's, asked for through `ext-background-effect-v1` on
winit's Wayland connection (`window_blur.rs`, the workspace's one allowance
of `unsafe`, for its two FFI calls).
The layout is a docked shell (`shell.rs`, plain geometry): a 36pt tab bar,
a 24pt status bar, and between them a 40pt icon rail, the sidebar, the
reading column (a 28pt breadcrumb over the pages) and the assistant, side
by side and split by hairlines; below 900pt the sidebar and the assistant
float over the pages. The background effect fills the reading column only.
The status bar carries the reading controls and a progress meter to scrub;
the page being read gets corner ticks and a meta line, and each pane a
column of page ticks at its right edge.
Split panes (`panes.rs`) divide the reading column with craft-ui's
docking model, one document per pane, each with its own header: its tree has tab ids as panels and
one panel per group, and the last session keeps it. The focused pane's tab
is the active tab, so commands, the status bar and the sidebar follow it; keys
and the gamepad reach only it, and pressing in another pane focuses that
one. Each tab has its own GPU view (target, transition snapshot, prepared
effects), kept for a few seconds after it was last drawn, and a document
shows in one pane at most, so a page's camera uniform serves one view per
frame. A fixed-layout document's pages can be turned and shown as a
two-page spread (`document.rs` lays out rows of turned page boxes and
converts between page and document coordinates); the camera turns them,
so nothing reloads, and the library's document preferences keep both. A
document reloads in place (same tab, same position) on the Reload command
or when its file changed while the window was in the background.

Every user action is a command in a registry (after pdfcraft's): an id,
a label, default keys and when it can run. Keys, the command palette
(Ctrl+K), menus and buttons all run commands through one dispatcher. Default
keys come from a keymap preset: Standard, or Vim (hjkl, `gg`/`G`, `Ctrl+W`
pane chords, `/` `n` `N`, `:` for the palette). A binding is a sequence of up
to three shortcuts (`G G`, `Ctrl+W V`), and no binding may start another, so
sequences need no timeout. In the Vim preset a count typed first repeats a
move, or names the page for `G`/`gg`. The user's shortcuts replace a
command's default keys in either preset and are kept by command id in the
Reader's preferences. They are edited in the Keyboard shortcuts tab
(`keymap_page.rs`), one of the page tabs (`pages.rs`) that sit in the tab
strip after the documents, with new tabs (`launcher.rs`, frame 04b), the
reading activity (`activity_view.rs`, frame 04c) and the library
(`library_view.rs`, frame 04d, whose table home and the new tab embed):
while one shows, no document is active, as on home (the `g` mark). These
views lay their content out in `glyphst_ui::layout::column`, the centred
column of the design's layout rules.

Translation runs the domain services on worker threads: a selection becomes a
card, and a PDF becomes a translation session whose pages (one-page PDFs
typeset by Typst, in a worker process of the reader's executable) are drawn
in place of the originals as they arrive. Each page has a source version
(original, or a preview revision); the loader and indexer load and read the
version shown, and a page's GPU drawing is replaced only when the new version
is ready, so the original stays visible meanwhile. Selection, copy and search
use the translated text; hovering a translated block shows the original.

The assistant runs the `ai` crate's agent on a worker thread with the main
model from the settings. The system prompt stays the same for a whole
conversation (so providers cache it and accept replayed reasoning); each user
message ends with a `<reader_state>` block for the document, page, section
and selection at that time. Tools come from a
[registry](crates/ai/src/tools.rs) in which each tool reads, navigates or
mutates; consecutive reads run in parallel. Reading tools
([`ai_tools`](crates/reader/src/ai_tools.rs)) open documents themselves with
MuPDF, by opaque ids (`doc_<tab id>`) the app keeps current. Tools that need
the app ([`ai_bridge`](crates/reader/src/ai_bridge.rs)) send a call that
`ReaderApp::logic` answers before the next frame (also while the window is
hidden). Mutating tools record their library edits as before/after snapshots
([`LibraryStore::record_edits`](crates/library/src/library/agent_edits.rs)) in
a [change log](crates/reader/src/ai_changes.rs): changes apply at once, and the
user reviews them, undoes or redoes each, or undoes all; an edit the user
changed since is left and reported. "Restore to here" on a user message
rewinds the chat to it and undoes that conversation's changes from then on.
Conversations are the library's AI sessions, saved with their whole trace
(reasoning, tool calls and results, images in the library's AI image store,
and providers' reasoning state), so a reopened chat continues as it was.
When a turn used 80% of the model's context window, the next one first
replaces the conversation with the model's summary (`AiSession.summary`), so
no earlier reasoning is replayed; a model names each chat after its first
answer. The panel ([`ai/header.rs`](crates/reader/src/ai/header.rs),
[`ai/thread.rs`](crates/reader/src/ai/thread.rs),
[`ai/composer.rs`](crates/reader/src/ai/composer.rs)) follows Zed's agent
panel for the composer and Lody's and Zeron's threads: user messages are
bubbles with copy, edit (in place; sending restarts from there) and restore
on hover; an answer's reasoning and tool calls form a timeline of
present-tense steps while it runs, folded into "Worked for 12s · searched 2×,
read 3 pages" once done. Messages written while an answer runs wait in a
queue. Mentions and pictures are chips in the composer's text
([`ai_editor`](crates/reader/src/ai_editor.rs)): runs of non-breaking text
coloured by a layouter, with plates painted under them and icons over their
leading room; an edit that touches a chip removes all of it, and the caret
steps over it. The `@` menu ([`ai_menu`](crates/reader/src/ai_menu.rs),
entries from [`ai_mentions`](crates/reader/src/ai_mentions.rs)) offers pages
as text or images, the selection, documents, outline sections, annotations
and library books, grouped, with mode words (`@section`, `@note`, `@doc`,
`@book`), `/` commands and the `+` actions (a picture file, a region). Beside
the menu, and on hovering a chip, a card previews what the chip attaches
([`ai_preview`](crates/reader/src/ai_preview.rs)): text cut at 4,000
characters, or page renders, read on worker threads and cached. What can
change in the reader is copied when mentioned, and pages are read on the
worker when the message is sent, into a `<context>` block; saved chats keep
the chips (`AiSessionMessage.mentions`), the time of each message and how
long each answer took. Pasted (`wl-paste`/`xclip`) and dropped pictures
become image chips. `--assistant-demo STATE` shows the panel in a canned
state (empty, menu, hover, running, done, edit, error, history) for
screenshots. External agents reach the same tools through an
[MCP server](crates/agent-host/src/mcp.rs) (streamable HTTP on 127.0.0.1,
plain JSON responses), started when first needed. Each agent session has its
own bearer token, which selects its tools and the origin and conversation its
changes are logged under; requests with a non-local `Origin` are refused.
`glyphst-reader --mcp-stdio` relays stdio JSON-RPC lines to it for agents
that only start MCP servers as commands. The `assistant.mcp_config` command
copies a config for an agent the user starts by hand. The assistant can also
run a chat on an external coding agent found on the machine
([`agent-host`](crates/agent-host/src/external.rs)): each turn starts the
agent's CLI in the document's working folder (under the data dir, a git
repository), resumes the agent's own session (saved with the chat), and gives
it a fresh MCP token for that turn. Claude Code runs over stream-json and
answers its permission requests itself: everything is allowed except file
writes outside the working folder. Codex runs over `codex app-server` with a
`workspace-write` sandbox on that folder and no approvals. Pi runs over
`pi --mode rpc` with a per-run extension that registers the MCP server and
blocks its `write` and `edit` tools outside the folder. Agents configured as
`acp_agent` nodes in the settings' `ai` section run over the Agent Client
Protocol (MCP over HTTP when they support it, otherwise the stdio relay);
their permission requests get "allow once". The folder is
checkpointed (a commit object kept by a `refs/glyphst/` ref, built with a
temporary index) before and after each turn, so the files a turn changed are
one entry in the change log that "Undo" and "Restore to here" put back.
Processes run in their own process group and are stopped with SIGTERM, then
SIGKILL, through `kill`. Shell tools of Claude Code and Pi, and every tool of
an ACP agent, can still write outside the folder; only Codex's sandbox
enforces it.
ChatGPT Subscription sign-in waits for the browser's callback, exchanges
the code and saves the credentials (in the keychain item the GPUI Reader
used, so its sign-in carries over) on worker threads, and the app repaints
when the state changes. Connection tests (AI, OCR) run on
threads, and a result for settings that changed meanwhile shows as stale.

A page still loading after 150 ms, with no thumbnail to show and at least
120 points a side on screen, gets the loading animation
([`generation.rs`](crates/reader/src/generation.rs), a port of
transitions.dev's UI generation): a dotted wireframe of the page's
structure, traced from a `PageSketch` the loader sends right after
capturing the page (before the thumbnail and tessellation), holds until
the page can be drawn and then reveals it behind a scan line. It is egui
shapes over the GPU view, so it never delays a page; with Reduce motion
the page fades in instead.

Page text comes from MuPDF structured text ([`pdf::text`](crates/pdf/src/text.rs))
with whitespace kept and ligatures expanded, so glyph offsets in reflowed
positions and highlight anchors saved by the GPUI Reader still resolve.
Text follows each loaded page; a second worker per document reads
the remaining pages' text for search and reflowed documents, and sidebar
thumbnails. Selections, search matches and highlights are egui shapes over
the GPU view. A selection is a run from one glyph to another, across
pages (the pages between whole), or a column on one page; the text of its
pages is kept, and read when missing, while it lasts. Text fields take
the keyboard focus once rather than every frame: egui's request_focus
also interrupts an input method's composition. Positions store pages from 1, as the library does. Fixed-layout
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
preserve data for. The activity log (`activity_v1`, `library/activity.rs`) is an additive
table of events (open, reading spans with their seconds, highlight, note,
bookmark, ask, translate), the newest 5,000 kept; home lists them and sums
the reading time. Reading sessions (`reading_sessions_v1`,
`library/reading.rs`) keep every span of reading, uncapped, with its start
and pages; an empty table starts from the log's reading spans. Home's
heatmap and the reading activity tab bucket them into local days
(`reader/src/reading_days.rs`). The Reader records both only when it
persists. First-page covers are rendered by MuPDF on a worker thread and
kept as JPEGs in the cache directory's `covers/` (`reader/src/covers.rs`).

When redb reports a file from an older file format
(`UpgradeRequired`), the library and the translation cache rename it aside as
`<name>.redb<version>-format` (never deleting it) and start a fresh database.

## Dependency maintenance

[Cargo.toml](Cargo.toml) and [Cargo.lock](Cargo.lock) pin crates.io
versions and Git revisions (craft-libs for craft-ui, merman for Mermaid).
Cargo and Nix use [rust-toolchain.toml](rust-toolchain.toml). egui, eframe,
egui_extras and craft-ui move together (craft-ui follows egui's releases),
and `pdf-gpu` uses egui's wgpu. Exact renderer/compiler/math pins and major
library upgrades require compatibility work.

The workspace's `../references/` holds read-only clones of upstream projects
(Highly-Efficient-PDF-Renderer, lucide, retain-pdf, typst, sioyek, zathura,
niri); they are not maintained forks, submodules or build inputs. To refresh
one, run `git -C ../references/<name> pull`. Preserve copied-source
and theme provenance independently of renames.

## Dependency audit

[The audit policy](.cargo/audit.toml) records the reviewed quick-xml exceptions
RUSTSEC-2026-0194 and RUSTSEC-2026-0195. At the October 11, 2026 checkpoint,
`quick-xml 0.38.4` remains through `citationberg 0.7.0 -> hayagriva 0.10.1 ->
typst-library 0.15.1`; Typst 0.15.1 is still the latest release. This is runtime CSL parsing. The Typst file loader limits
project/package access but does not generally bound XML CPU or allocation work.

A configured audit pass does not mean the dependency graph is warning-free.
Exception removal and remaining warning paths are tracked in
[ROADMAP](ROADMAP.md#2-dependency-audit-follow-up); refresh the advisory database
before treating old results as current security evidence.
