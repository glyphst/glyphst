# Next work

A short ordered shortlist for tasks without a more specific user direction.
Select one bounded step, verify its current premise, and finish its acceptance
checks. Update affected entries when evidence changes; remove completed work
rather than appending a progress history. Evidence below was reviewed on
2026-10-11 (entry 1 on 2026-10-12); each entry identifies its relevant
checkpoint or evidence.

Use [architecture](ARCHITECTURE.md) for boundaries and
[verification](README.md#verification) for commands.

## 1. egui Reader: M2 working with text

**Owner:** `reader-egui`, `library`.

**Current evidence:** egui hosts the GPU vector renderer (GPUI 0.3.8 cannot
composite a wgpu texture on Linux); the user confirmed smooth zoom and fcitx5
input in the spike. M1 of the [egui Reader](crates/reader-egui) is in place:
a library home (history and library items, an async open dialog, dropped
files), [craft-ui](https://github.com/storytold/craft-libs) tabs, the GPU
document view with outline, page and zoom controls, presentation mode,
positions in the shared library, a System/Light/Dark theme after pdfcraft,
and [effects](crates/effects) for view transitions (tab switches, page turns)
and backgrounds. The app is not a 1:1 port of the GPUI Reader: artcraft's
egui apps are the reference, and old crates are reused only where cheap.

Measured on a 120 Hz display (Intel Arc): `--bench` zooms Murietta_Level1,
Lower Level and War and Peace 1x-64x-1x with no frame over 25 ms (p95
8.6-8.7 ms) and scrolls War and Peace (1,259 pages) with p95 9.0 ms and at
most one frame just over 25 ms. First pages appear after 0.15 s (War and
Peace), 1.3 s (Murietta) and 9.4 s (Lower Level); idle CPU is 0.0% with the
solid background and 6% with the animated aurora at its 30 fps cap. Images
and image masks are textures at their own resolution; the remaining raster
layers (blend groups, soft masks, shadings, patterns) are re-rasterized for
the view once it has been still for 150 ms. The pdf.js corpus tier renders 97
of 100 files (2 unreadable, 1 parser timeout); 20 pages still use raster
layers and 1 differs from MuPDF by more than 1%.

Known gaps: Lower Level (7.6M strokes) costs one 250 ms frame to upload its
level buffers (uploading levels across frames needs level fallback); 4x MSAA gives small text coarser edges
than MuPDF; only fixed-layout pages are shown (EPUB reflow is M2).

**Next step (M2):** text selection and copy, search, internal and external
links, bookmarks/highlights/notes from the library, a thumbnail sidebar and
EPUB reflow, taking pdfcraft's search and side panels as the reference.
Later milestones: M3 translation and AI panels, M4 settings, keys and command
palette, M5 split panes (craft-ui docking), the `files` app and removing the
GPUI crates, M6 an effects manager (user effect directory, hot reload,
previews, per-book choices).

**Done when:** M2's features work in the egui Reader with tests, `check.sh
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
