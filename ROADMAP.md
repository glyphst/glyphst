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
  tabs, the GPU document view, presentation mode, reading positions, text
  selection, find, links with history, bookmarks/highlights/notes, a
  sidebar, EPUB reflow, selection and whole-document translation, an
  assistant (streaming answers, document tools, sessions in the library), a
  command registry and palette, configurable shortcuts and a settings
  window; [effects](crates/effects) for transitions and backgrounds.
- **Styling:** pdfcraft's components (tokens, Inter, Lucide icons, pills,
  cards, toasts) and a reading layout of its own, the stage: the
  background effect behind the whole window, floating drawers opened from
  a rail, a dock of reading controls that fades while reading, a progress
  line with chapter scrubbing, page shadows, and a home page with the
  book to continue and cover cards.
- **M5:** page uploads spread across frames; split panes (a tab dragged
  onto a pane's edge, or Split right/down, opens beside it; each pane has a
  title chip, the focused one an outline, dividers resize them, and the
  last session keeps them); two-page spreads, rotation (both kept per
  document) and reloading changed files; ChatGPT Subscription sign-in, the
  AI providers' order and AI and OCR connection tests.
- **M6:** the effects manager: Settings › Effects with live preview cards
  for backgrounds, transitions and page tints, parameter sliders, an image
  wallpaper (Wallpaper, Soft and Parallax backgrounds) and the effects
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
- A very dense page is slow to load: Lower Level (7.6M strokes) takes
  about 10.5 s before its upload starts (MuPDF's display list 7.0 s,
  capture 1.9 s, preparing levels 1.4 s). Once uploading it shows its
  coarsest level after 49 ms and sharpens over 1.6 s of 4 MiB frames.
- 4x MSAA gives small text coarser edges than MuPDF.
- Presenting a spread shows one page at a time.
- A relayout of a large EPUB blocks the window while MuPDF lays it out.
- Translation sessions are not restored from the cache on reopening.
- The assistant has no Mermaid diagrams (`mermaid-render` is kept for
  them). A region is one page's; the overview selects no pages for it.
  The change log of agents' edits lives while the app runs (saved chats
  keep their trace, not their undo list), and picture pasting needs
  `wl-paste` (Wayland) or `xclip` (X11).
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
- The wallpaper is written to the GPU in one go when chosen (a 4K image
  can cost a frame then), and Solid colour's preview card shows its
  manifest colour rather than the theme's.

**Next step:** the user picks the next milestone. Candidates from the gaps
above: restoring translation sessions from the cache, Mermaid diagrams in
the assistant, and loading very dense pages sooner (MuPDF's display list
dominates).

**Done when:** the chosen milestone works with tests, `check.sh quick`
passes, and the bench still shows no frames over 25 ms.

## 2. Dependency audit follow-up

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
