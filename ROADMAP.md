# Next work

A short ordered shortlist for tasks without a more specific user direction.
Select one bounded step, verify its current premise, and finish its acceptance
checks. Update affected entries when evidence changes; remove completed work
rather than appending a progress history. Evidence below was reviewed on
2026-10-13; each entry identifies its relevant
checkpoint or evidence.

Use [architecture](ARCHITECTURE.md) for boundaries and
[verification](README.md#verification) for commands.

## 1. Reader: the next milestone

**Owner:** `reader`.

**Current evidence:** The [Reader](crates/reader) runs on egui with the GPU
vector renderer; it replaced the GPUI Reader (removed with the GPUI crates
and the abandoned Files app in M5). It has:
- **M1-M4:** a library home, [craft-ui](https://github.com/storytold/craft-libs)
  tabs, the GPU document view, presentation mode (fullscreen, a tool ring,
  ink, the laser and a clock), reading positions, text
  selection, find, links with history, bookmarks/highlights/notes, a
  sidebar, EPUB reflow, selection and whole-document translation, an
  assistant (streaming answers, document tools, sessions in the library), a
  command registry and palette, configurable shortcuts and a settings
  window; [effects](crates/effects) for transitions and backgrounds.
- **Styling:** the glyph design (`design/main.pen`) on the `ui` crate's
  components: mono chrome, hairline panes docked beside the pages, a
  status bar in place of the dock and progress line, rule titles and tree
  branches, first-page covers, a reading heatmap and activity log on home,
  new tabs with a launcher, a reading activity tab, and new appearance
  settings (accent, fonts, ui scale, density, ornaments).
- **M5:** page uploads spread across frames; split panes (a tab dragged
  onto a pane's edge, or Split right/down, opens beside it; each pane has a
  title chip, the focused one an outline, dividers resize them, and the
  last session keeps them); two-page spreads, rotation (both kept per
  document) and reloading changed files; ChatGPT Subscription sign-in, the
  AI providers' order and AI and OCR connection tests.
- **M6:** the effects manager: Settings › Effects with live preview cards
  for backgrounds, transitions and page tints, parameter sliders, an image
  wallpaper (now one Wallpaper background with dim, blur and parallax,
  set in Appearance) and the effects
  folder (open it, write a commented starter effect, see load errors);
  user effects reload when their files change; the Effects drawer gives a
  book its own background, transition and tint; page tints Sepia, Night
  and Paper grain.
- **M7:** dense pages show their coarsest level while the rest uploads
  (Lower Level after 49 ms rather than 1.6 s); the page overview (Ctrl+G,
  a zoomable grid with sharp renders of large cells); zen mode (Z);
  region selection (S: copy or save a page area as an image, copy or
  translate its text, or ask about it); quick ask (Tab, one question
  without the chat's history, continued in the assistant if wanted);
  half-page scrolling (Shift+Page Down/Up) and working arrow keys; a tab
  context menu; thumbnails turned with their pages. As in the GPUI Reader,
  the sidebar outline follows the section being read (scrolling it away
  pauses following; the locate button resumes it), its entries collapse
  and their parents stick at the top, and the thumbnails follow the page.
- **Library (2026-10-19):** papers and books are managed as in Zotero:
  - collections an item can share and linked files, in a folder the user
    chooses;
  - metadata from the DOI, arXiv id or ISBN on the first pages (arXiv,
    Crossref, OpenAlex, Open Library), and duplicates found;
  - the library model's pass (summary, key points, tags, a collection),
    run on new items and asked before the backfill, with every AI tag
    reviewable;
  - a Markdown vault Obsidian can open, with tag edits flowing back;
  - adding by identifier and importing from Zotero (notes, PDF
    highlights);
  - the agents' library tools. Frames 04d-04k, 06d, 03b and 07-library.
- **Before M7:** selections run across pages (a drag follows the pointer
  onto other pages, Shift+click extends; copy, highlights, notes,
  translation and the assistant get all of it); typing Chinese or
  Japanese in the command palette no longer resets the input method's
  composition.

Measured on a 120 Hz display (Intel Arc): `--bench` zooms Murietta_Level1,
Lower Level, War and Peace and the 15-page image-heavy brochure 1x-64x-1x,
scrolls War and Peace (1,259 pages) and the brochure, and searches each
whole document (48,603 matches for "the" in War and Peace) with no frame
over 25 ms (worst 21 ms, p95 8.5-9 ms), also with War and Peace as a
spread or turned, with the Night tint, and with the Soft wallpaper and
Sepia; split side by side with Murietta, one search frame took
25.0 ms in one of three runs. Uploads go through a
reused staging belt, 4 MiB per frame, and GPU memory comes in 8-32 MiB
blocks. With a fake document translation running alongside, 12 brochure
pages swap in with 0-1 scroll frames over 25 ms. The pdf.js corpus tier: 97
of 100 files render, 1 differs from MuPDF by more than 1%; turned 90
degrees, 20 of them differ from MuPDF as little as upright.

Known gaps:
- A very dense page is slow to finish loading the first time: Lower
  Level (7.6M strokes) shows previews of what MuPDF has interpreted from
  about 0.5 s, but the page itself is drawn after about 11 s. MuPDF's run
  over the page dominates (in a profile, reading and lexing the 51 MB
  content stream 41%, inflate 15%), then preparing levels (1.3 s).
  Reopened, it comes from the page cache: drawn after about 3.3 s
  (decoding 0.6 s, preparing levels 1.4 s); caching the prepared levels
  too (579 MiB for Lower Level) would cut that further. Measure on the
  performance cores (`taskset -c 0-11`): a loader thread on a low-power
  core takes twice as long.
- Presenting a spread shows one page at a time.
  Presenting's ink lives while its tab is open (never saved), and it has
  no spotlight, zoom to a region or presenter view on a second screen.
- A relayout of a large EPUB blocks the window while MuPDF lays it out.
- Translation sessions are not restored from the cache on reopening.
- A region is one page's; the overview selects no pages for it.
  The change log of agents' edits lives while the app runs (saved chats
  keep their trace, not their undo list), and picture pasting needs
  `wl-paste` (Wayland) or `xclip` (X11).
- Assistant panel: chips of a reopened chat carry what was kept when sent (a
  selection's text, a page's label), not the live pages; streamed text
  appears without a fade, and the empty state's composer does not glide to
  the bottom on the first message; the fake provider streams no reasoning,
  so `--ai-fake` runs show no thoughts (`--assistant-demo` does).
- External agents: only Codex's sandbox keeps writes in the document's
  working folder (Claude Code's and Pi's shells, and ACP agents, are not
  confined); ACP agents are configured in `config.kdl` only, with no
  settings page; each turn starts the agent's CLI anew (no steering of a
  running turn); the agent's model is its own (no picker).
- Dropped with the GPUI Reader: multiple windows and tearing tabs off,
  synchronized panes, the publisher-styles toggle, and translated (i18n)
  interface text.
- M7's pointer work (dragging regions, the overview's clicks and
  Ctrl+wheel, the tab menu, typing into quick ask with an input method)
  is checked by screenshots and unit tests, not yet by hand.
- No automated run writes the real keychain or signs in to ChatGPT: the
  user's checks that a key set in the Reader is found again after a
  restart, and of a ChatGPT Subscription sign-in, are pending. Editing an
  effect file while the Reader runs (the reload) is also checked by hand.
- Library: the Zotero import leaves out tag colours and related items
  (no tables for them yet), EPUB annotations and rotated pages'
  highlights. Duplicates are found but not merged. Agents have no
  `lookup_metadata` or vault-block tool. The AI pass, the lookups and the
  import are checked by tests on fakes and by screenshots; a real Zotero
  library and real providers are not yet checked by hand.
- The wallpaper is written to the GPU in one go when chosen (a 4K image
  can cost a frame then), and Solid colour's preview card shows its
  manifest colour rather than the theme's.

**Next step:** the user picks the next milestone. Candidates: the
library's M2, local hybrid search over every item's text with
EmbeddingGemma 2 (a spike of llama.cpp against `ort` first); then, from
the gaps above, restoring translation
sessions from the cache, and a dense page's
first load (MuPDF's interpretation
dominates; only a faster content-stream reader would cut it).

**Done when:** the chosen milestone works with tests, `check.sh quick`
passes, and the bench still shows no frames over 25 ms.

## 2. pdf-gpu: follow HEPR's renderer

**Owner:** `pdf-gpu`.

**Current evidence:** Highly-Efficient-PDF-Renderer (the workspace's
`../references/Highly-Efficient-PDF-Renderer`, read at v0.1.65, 3c2dbd1) is
the model for `pdf-gpu`'s zoom. Ported so far: the margin selection
(ff69ebb), thumbnails drawn from GPU pages, analytic fill coverage
(8d41b7b, 39848f9; bands as in `vectorFillBands.ts`) and fills and strokes
in one ordered stream and pipeline (9debd42). Coverage brought the pdf.js
corpus closer to MuPDF (mean difference at 1x, 90th percentile, 1.83 to
1.71) and cut text-page preparation (tracemonkey 79 to 29 ms). By GPU pass
time (`zoom_bench` `gpu_*_ms`, timestamps, median of two 240-frame runs) it
costs little on drawings (Murietta p95 1.85 to ~2.0 ms, Lower Level
unchanged) but text: tracemonkey p50 0.38 to 0.60 ms, 0.33 to 0.68 ms
between 0.25x and 1x, from per-pixel edge loops. Spreading the ink of paths
under 2.5 px (HEPR's small-glyph rule) did not help: most glyphs at those
zooms are larger.

What a text pixel costs (`zoom_bench` `coverage_work`, a CPU model of
each pixel's edge reads): at 0.25x-1x a tracemonkey pixel sums 13-17
edges of glyphs of 19-28 edges, so neither HEPR's cell index (13-18:
HEPR itself indexes only paths of 24+ segments) nor quadratic edges
(24-47 line-equivalents: coarse levels flatten a glyph into fewer lines
than it has curves) would cut it. Mesa's per-draw times
(`INTEL_MEASURE=draw`) at 0.5x put the text draw at 54-75 us with lyon
and 577-909 us with coverage (first frames). Even with edges made up in
the shader instead of read it takes 244-377 us. So it is arithmetic: each
edge is ~30-40 instructions, and on this small GPU (Intel Arc, Meteor
Lake) glyph triangles of a few pixels leave most of each SIMD16 thread
empty. These did not help: reordering same-colour fills (pixel-shader
ordering is not the cause), reading edges from a texture as HEPR does,
reading four edges ahead, skipping pixels outside the path, and a tighter
quad margin. One triangle per glyph instead of two may save ~10%.
Tessellating fills under 8 px on the levels serving scales up to 1
restores the GPU time below 0.5x (0.79 to 0.38 ms at 0.25x) but not at
1x, and takes text further from MuPDF (0.06 to 0.21 at 0.25x). GPU time
varies ~30% run to run with the clock (800-1,650 of 2,350 MHz):
compare variants alternately, over several runs.

Still to port or find:
- HEPR's glyph raster atlas (`textRasterAtlas.ts`): each distinct glyph
  drawn once at ~96 px and sampled when small, which needs glyph identity
  (font and glyph id) kept through capture.
- HEPR's cell index (`vectorCellIndex.ts`, `vectorCellShaders.ts`) for
  large drawing paths; it does not bound glyphs.
- Per-pixel clip tests instead of stencil push/pop (`vectorClipShaders.ts`):
  Murietta's remaining draws are 1,644 stencil clip pairs per level.
  Coverage clips would also allow dropping MSAA.
- Reordering paints that commute (`vectorPageDrawScheduler.ts`), reusable
  stencil patterns (b6a755c), and smoothed one-pixel stroke coverage for
  thumbnails (178588b): GPU thumbnails of Murietta are darker than MuPDF's.

**Next step:** the user decides between a glyph atlas for small text and
accepting the text cost (about 0.2-0.35 ms GPU at 0.25x-1x, inside a
120 Hz frame), measured by `zoom_bench` GPU time on tracemonkey, Murietta
and Lower Level.

**Done when:** tracemonkey's GPU p50 is within 10% of its 0.38 ms before
coverage fills, the drawings are no slower, and the corpus keeps its mean
differences.

## 3. Dependency audit follow-up

**Owner:** Workspace dependency maintenance.

**Current evidence:** [.cargo/audit.toml](.cargo/audit.toml) and the
[architecture rationale](ARCHITECTURE.md#dependency-audit) retain two quick-xml
exceptions. An audit on 2026-10-13, after GPUI was removed, reports eight
allowed warnings: unmaintained `bincode`, `cgmath`, `paste`, `rustybuzz`,
`ttf-parser`, `unsafe-libyaml` and `yaml-rust`, plus cgmath advisory
RUSTSEC-2026-0197 (`instant` and `rustls-pemfile` went with GPUI). quick-xml
0.38.4 is still reachable only through Typst 0.15.1, the latest release.

**Next step:** Refresh advisories and identify the currently reachable dependency
paths. When the pinned Typst chain supports a fixed quick-xml version (at least
0.41 for the reviewed advisories), make the smallest compatible update and
remove the corresponding exceptions. Handle remaining warnings by owner/path.

**Done when:** An audit without the removed exceptions confirms the fix, locked
workspace checks pass, and any remaining warnings retain accurate dispositions.
Do not add ignores merely to obtain a clean summary.
