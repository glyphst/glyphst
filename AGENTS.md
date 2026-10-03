# Glyphst

Rust desktop applications using GPUI and GPUI Kit's `gpui-component` facade.

## Working agreements

- Carry the requested work through implementation and relevant verification.
  Resolve routine choices; ask when missing information changes correctness or scope.
- Prefer existing code and dependencies. Keep updates concise and distinguish
  completed checks, baseline failures, and remaining limitations.
- When asked to choose work, use [ROADMAP.md](ROADMAP.md). Verify the selected
  entry against current code and evidence; update affected entries and remove
  completed ones. An explicit user task takes precedence over the shortlist.

## Project constraints

- `reader` and `files` are entry points; shared UI, service, and domain owners
  are described in [ARCHITECTURE.md](ARCHITECTURE.md).
- `../glyphst-pdf` owns the standalone PDF engine. Keep GPUI integration in
  `crates/pdf-gpu` and MuPDF in `crates/pdf`. Unsupported native semantics
  require whole-page fallback.
- Preserve task ownership, cancellation generations, paint order, resource
  limits, and performance/quality gates when changing GPU work.
- The library's production current-format opener also serves tests through
  `LibraryStore::open_for_test`. Cleanup must not change on-disk formats or
  restore obsolete migrators.
- Compiled-page caches have count and estimated-byte limits. The engine's
  `COMPILED_PAGE_CACHE_VERSION` controls their namespace; encrypted documents
  must never use the compiled-page disk cache.
- Normal builds use published Git pins. Preserve the GPUI Kit Rust aliases and
  aligned GPUI revisions; follow the [dependency policy](ARCHITECTURE.md#dependency-maintenance)
  when working on maintained forks. `references/` contains read-only reference
  submodules, distinct from maintained sibling checkouts.
- Do not build Nix derivations unless explicitly requested. Do not use cua-driver
  on this Niri development environment.

## Verification and documentation

Workspace-wide rules (single active session, check tiers, evidence, disk) are in
`../AGENTS.md`. Run `script/check.sh quick` for code changes and
`script/check.sh gate` before a PR or engine pin bump; add `--local` to build
against sibling checkouts. Documentation-only edits need link/content checks.

Keep docs about current contracts and next actions; put run summaries in the
commit message or PR, not in the source tree.
