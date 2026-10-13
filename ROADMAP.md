# Next work

A short ordered shortlist for tasks without a more specific user direction.
Select one bounded step, verify its current premise, and finish its acceptance
checks. Update affected entries when evidence changes; remove completed work
rather than appending a progress history. Evidence below was reviewed on
2026-10-11 (entry 1 on 2026-10-13); each entry identifies its relevant
checkpoint or evidence.

Use [architecture](ARCHITECTURE.md) for boundaries and
[verification](README.md#verification) for commands.

## 1. egui Reader: M5 split panes, the GPUI-only features, removing GPUI

**Owner:** `reader-egui`, `files`, `workspace`.

**Current evidence:** egui hosts the GPU vector renderer (GPUI 0.3.8 cannot
composite a wgpu texture on Linux); the user confirmed smooth zoom and fcitx5
input in the spike. The [egui Reader](crates/reader-egui) has:
- **M1:** a library home, [craft-ui](https://github.com/storytold/craft-libs)
  tabs, the GPU document view, presentation mode, shared reading positions,
  a theme after pdfcraft, and [effects](crates/effects) for transitions and
  backgrounds.
- **M2:** text selection and copy, find and a search panel, links with
  history, bookmarks/highlights/notes shared with the GPUI Reader, a
  sidebar, and EPUB reflow.
- **M3:** selection translation cards, whole-document translation with
  translated pages swapped in as they are typeset, and an assistant panel
  (streaming answers, page and selection context, document tools, sessions
  in the library). `settings` and `ai` build without GPUI.
- **M4:** a command registry behind keys, menus, buttons and a command
  palette (Ctrl+K); configurable shortcuts kept in the egui Reader's
  preferences; a settings window (appearance, reading, shortcuts,
  translation, OCR, AI providers and models) that saves the provider
  sections and keys where the GPUI Reader keeps them; and a model picker in
  the assistant.
- **Styling:** pdfcraft's components (tokens, Inter, Lucide icons, pills,
  cards, toasts) and a reading layout of its own, the stage: the
  background effect behind the whole window, floating drawers opened from
  a rail, a dock of reading controls that fades while reading, a progress
  line with chapter scrubbing, page shadows, and a home page with the
  book to continue and cover cards.
- **M5 so far:** page uploads spread across frames; split panes (a tab
  dragged onto a pane's edge, or Split right/down, opens beside it; each
  pane has a title chip, the focused one an outline, dividers resize them,
  and the last session keeps them); two-page spreads, rotation (both kept
  per document) and reloading changed files.

The app is not a 1:1 port of the GPUI Reader: artcraft's egui apps are the
reference, and old crates are reused only where cheap.

Measured on a 120 Hz display (Intel Arc): `--bench` zooms Murietta_Level1,
Lower Level, War and Peace and the 15-page image-heavy brochure 1x-64x-1x,
scrolls War and Peace (1,259 pages) and the brochure, and searches each
whole document (48,603 matches for "the" in War and Peace) with no frame
over 25 ms (worst 21 ms, p95 8.5-9 ms), also with two documents split
side by side. Uploads are spread across frames
(4 MiB each, through a reused staging belt) and GPU memory comes in small
blocks: a new 256 MiB block used to cost a 16-23 ms submission, and page 19
of War and Peace a 27 ms upload. With a fake document translation running
alongside, 12 brochure pages swap in with 0-1 scroll frames over 25 ms. The
pdf.js corpus tier is unchanged: 97 of 100 files render, 1 differs from
MuPDF by more than 1%; turned 90 degrees, 20 of them differ from MuPDF as
little as upright.

Known gaps:
- A very dense page shows only once all of it is uploaded: Lower Level
  (7.6M strokes) takes about 200 ms of 4 MiB frames before it appears.
- 4x MSAA gives small text coarser edges than MuPDF.
- A turned page's loading placeholder (its thumbnail) shows upright, and
  presenting a spread shows one page at a time.
- A relayout of a large EPUB blocks the window while MuPDF lays it out.
- Selections stay within one page.
- Translation sessions are not restored from the cache on reopening.
- The assistant has no image or region mentions, no Mermaid diagrams and no
  ChatGPT Subscription sign-in.
- Settings saved from the egui Reader are overwritten by a GPUI Reader that
  is running at the same time; provider order and connection tests are
  GPUI-only. No automated run writes the real keychain: the user's check of
  a key set in the egui Reader showing in the GPUI Reader is pending.

**Next step (M5):** ChatGPT Subscription
sign-in, provider order and connection tests in the settings window; then
removing the GPUI crates and the `files` app (abandoned, not ported).
Later: M6 an effects manager (user effect directory, hot reload, previews,
per-book choices).

**Done when:** M5's features work in the egui Reader with tests, `check.sh
quick` and `corpus` pass, and the bench shows no frames over 25 ms.

## 2. Workspace layout smoke test

**Owner:** `workspace`.

**Current evidence:** Panes and tabs run on the GPUI Kit dock (`DockArea`
center, `TabItem` panels, the `pane::render` skin; GPUI Kit fork patch
`dock: Let the host place, move and veto panel drops`). The session format is
unchanged. Workspace tests drive tab drops, edge splits, synchronized-group
vetoes, swaps, moves to a border, keyboard resizing, closing, restore and a
move between windows through the host drop path. They do not cover pointer
drags through the dock's drop zones, tear-off on Wayland, or the look of the
pane cards and drop previews. The side docks stay the workspace's own: they
show the active document's sidebar and the AI panel, not dockable panels.

**Next step:** The user runs `cargo run -p glyphst-reader --release -- <pdf>`
and checks splits; dragging tabs within a pane, between panes and to edges;
tearing a tab off and dragging it back; synchronized tabs and their lines;
zen mode; the tab context menu; a password-protected PDF; and session restore
after quitting.

**Done when:** that smoke test passes, or its findings are fixed with tests.

## 3. Long-document coordinate precision

**Owner:** `document-ui` layout and viewport geometry.

**Current evidence:** [Page geometry](crates/document-ui/src/document_view/geometry.rs)
subtracts document scroll coordinates in f64, but page offsets and other layout
values still originate as f32, which is not proven pixel-stable at maximum zoom
deep in a long document.

**Next step:** Add a focused numerical reproduction with large cumulative page
offsets, then keep layout origins at sufficient precision until viewport-relative
conversion. Preserve rotation, fractional device origin, and camera identity.

**Done when:** Equivalent near-origin and distant-page crops agree at extreme
zoom, with round-trip scroll/zoom restoration passing. Do not infer correctness
solely from ordinary first-page rendering.

## 4. Dependency audit follow-up

**Owner:** Workspace dependency maintenance.

**Current evidence:** [.cargo/audit.toml](.cargo/audit.toml) and the
[architecture rationale](ARCHITECTURE.md#dependency-audit) retain two quick-xml
exceptions. The reviewed audit also reports unmaintained `bincode`, `cgmath`,
`instant`, `paste`, `rustls-pemfile`, `rustybuzz`, `ttf-parser`, and `yaml-rust`,
plus cgmath advisory RUSTSEC-2026-0197. A refreshed audit on 2026-10-11, after
the dependency upgrades, reports the same nine allowed warnings; quick-xml
0.38.4 is still reachable only through Typst 0.15.1, the latest release.

**Next step:** Refresh advisories and identify the currently reachable dependency
paths. When the pinned Typst chain supports a fixed quick-xml version (at least
0.41 for the reviewed advisories), make the smallest compatible update and
remove the corresponding exceptions. Handle remaining warnings by owner/path.

**Done when:** An audit without the removed exceptions confirms the fix, locked
workspace checks pass, and any remaining warnings retain accurate dispositions.
Do not add ignores merely to obtain a clean summary.
