# Glyphst

Glyphst Reader, a Rust PDF and EPUB reader on egui and wgpu (GPU vector
pages through `pdf-gpu`), with artcraft's craft-ui components copied into
`glyphst-ui` (`glyphst_ui::craft`).

## Working agreements

- Carry the requested work through implementation and relevant verification.
  Resolve routine choices; ask when missing information changes correctness or scope.
- Prefer existing code and dependencies. Keep updates concise and distinguish
  completed checks, baseline failures, and remaining limitations.
- When asked to choose work, use [ROADMAP.md](ROADMAP.md). Verify the selected
  entry against current code and evidence; update affected entries and remove
  completed ones. An explicit user task takes precedence over the shortlist.

## Project constraints

- `reader` is the entry point; service and domain owners are described in
  [ARCHITECTURE.md](ARCHITECTURE.md). The egui apps in `../../artcraft` are
  the reference for UI patterns.
- UI follows the pen.dev design in `../design` (see the workspace
  `../AGENTS.md` for how to read it).
- MuPDF renders every document page; keep its integration in `crates/pdf`.
- Preserve task ownership, cancellation, paint order, and render-cache limits
  when changing page rendering.
- The library's production current-format opener also serves tests through
  `LibraryStore::open_for_test`. Do not add migrators; a format change must keep
  the old file (see the library persistence notes in ARCHITECTURE.md).
- Normal builds use the published revisions in `Cargo.toml` and `Cargo.lock`
  (see the [dependency policy](ARCHITECTURE.md#dependency-maintenance)).
  The workspace's `../references/` holds read-only upstream clones.
- Do not build Nix derivations unless explicitly requested. Do not use cua-driver
  on this Niri development environment.

## Verification and documentation

Workspace-wide rules (single active session, check tiers, disk) are in
`../AGENTS.md`. Run `script/check.sh quick` for code changes, plus
`script/check.sh screens <frames> --theme dark` for the frames a UI change
touches, `script/check.sh corpus` for page rendering changes, and
`script/check.sh gate` with the full `screens` before a PR.
Documentation-only edits need link/content checks.

Keep docs about current contracts and next actions; put run summaries in the
commit message or PR, not in the source tree.
