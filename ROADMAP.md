# Next work

A short ordered shortlist for tasks without a more specific user direction.
Select one bounded step, verify its current premise, and finish its acceptance
checks. Update affected entries when evidence changes; remove completed work
rather than appending a progress history. Evidence below was reviewed on
2026-10-11 (entry 1 on 2026-10-12); each entry identifies its relevant
checkpoint or evidence.

Use [architecture](ARCHITECTURE.md) for boundaries and
[verification](README.md#verification) for commands.

## 1. GPU vector page rendering: GPUI or egui

**Owner:** `pdf-gpu`, `egui-spike`, `document-ui`.

**Current evidence:** Captured MuPDF pages render on the GPU with every frame
drawn from vectors or from an overview rendered from them. On this machine's
Intel Arc iGPU, `zoom_bench` zooms Murietta_Level1, Level 1, Lower Level and
War and Peace page 1 from 0.25x to 64x with p95 frame times of 0.6-3.1 ms
(MuPDF needs 0.5-2.6 s per fit-size view of the drawings). Fewer than 1.5% of
pixels differ from MuPDF at fit and under 0.07% at 8x and 64x. The pdf.js
corpus tier renders 97 of 100 files (2 unreadable, 1 parser timeout); 2 differ
by more than 1%.

GPUI 0.3.8 cannot composite a wgpu texture on Linux (`PaintSurface` is a
no-op). The [egui spike](crates/egui-spike) (eframe 0.36, wgpu 30, the
photocraft paint-callback pattern) hosts the renderer unchanged. On a 120 Hz
display its `--bench` mode zooms Murietta 1x-64x-1x with no frame over 25 ms
(p95 8.6 ms intervals) and scrolls 600 frames through War and Peace (1,259
pages, pages loaded on demand within a 1 GiB budget, 126 MiB used) with p95
9.0 ms and 2 frames over 25 ms, from page uploads. The first page is drawn
about 250 ms after start. Idle, it uses 0.1% CPU (the GPUI Reader 0.6%) and
196 MB RSS; its stripped binary is 31 MB (the Reader's, with far more
features, 125 MB). CJK labels and text fields render with the system Noto
Sans CJK through fontconfig, and a custom-shader slide-over page transition
runs inside egui's pass (`--screenshot-transition`).

Known renderer gaps: raster layers are rasterized once (blurry when zoomed
deep; images and image masks should become native-resolution textures), 4x
MSAA gives small text coarser edges than MuPDF, zooming into Lower Level's
dense hatching reaches 27 ms frames, a dense page's first upload can take a
frame of 30-180 ms, and GPU memory reaches 580 MiB for the largest drawing.

**Next step:** The user tries `cargo run --release -p glyphst-egui-spike --
<pdf>`: typing Chinese and Japanese with fcitx5 in both text fields, the
presentation-mode transition (P, then Page Down), a gamepad if available, and
zooming Murietta_Level1 against HEPR's demo. With those results, choose the
host: port the Reader to egui, or patch GPUI with a wgpu `PaintSurface`.

**Done when:** The Reader zooms Murietta_Level1 without a raster swap,
`script/check.sh corpus` passes, and multi-page memory is bounded.

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
