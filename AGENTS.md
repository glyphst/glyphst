# Glyphst

Glyphst is a workspace for desktop applications built with Rust, GPUI, and GPUI Kit's
`gpui-component` compatibility facade. See `ARCHITECTURE.md` for ownership boundaries.

## Repository Layout

- `crates/reader` and `crates/files` contain the application binaries.
- The other crates in `crates/` contain shared UI, application services, and domain code.
- `../gpui-kit` is the independent maintained UI fork; normal builds use its published Git pin.
- `../glyphst-pdf` is the standalone PDF engine. `crates/pdf-gpu` is its GPUI adapter;
  `crates/pdf` owns the MuPDF integration. Keep GPUI and MuPDF out of the standalone engine.
- `../zed` is the maintained GPUI fork, distinct from the read-only `references/zed`.
- `references/` contains shallow, read-only reference repositories: Zed, Zathura, Sioyek,
  Catppuccin, Cherry Studio, Comet, Lucide, Niri, Retain PDF, Typst, and Yazi.

Update a reference with `git submodule update --remote --depth 1 <path>`, then review and commit its
gitlink.

## GPUI Kit Policy

- Keep `main` as a fast-forward-only mirror of `upstream/main`.
- Maintain Glyphst changes on `glyphst/main` and merge `upstream/main` into it regularly.
- Keep composable widgets, styling workarounds, and application-specific keybindings in Glyphst.
- Change the fork only when a feature requires GPUI Kit private internals or must alter a
  component's internal event or render lifecycle.
- Preserve the `gpui_component`, `gpui_component_assets`, and `gpui_fps` Rust aliases.
- Keep GPUI revisions aligned across Glyphst and GPUI Kit. Publish Zed, then Kit/PDF,
  then update Glyphst's Git pins. Local overrides are opt-in as described in `README.md`.

## Validation and State

- Use `rust-toolchain.toml`, including in Nix. Run workspace fmt, Clippy with `-D warnings`,
  and all-target/all-feature tests with `--locked`; review `cargo audit` exceptions.
- Library tests use `LibraryStore::open_for_test`, which delegates to the production current-format
  opener. Do not restore obsolete migrators or change current on-disk formats during cleanup.
- GPU module splits must retain task ownership, cancellation generations, ordering, and whole-page
  fallback. Keep performance/quality gates unchanged and distinguish baseline failures.
- The compiled-page cache has count and estimated-byte limits. Its version comes from the PDF
  engine's `COMPILED_PAGE_CACHE_VERSION`; encrypted documents must not use its disk cache.

## Development Constraint

1. Do not build Nix derivations unless explicitly requested.
2. Do not use cua-driver since it doesn't support Niri wayland compositor (our dev env) well.
