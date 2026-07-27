# Glyphst

Glyphst is a desktop PDF and EPUB reader built with Rust and GPUI.

## Repository Layout

- `crates/` contains the application and domain crates.
- `dependencies/gpui-component` is the maintained gpui-component fork used by Glyphst.
- `references/` contains shallow, read-only reference repositories: Zed, Zathura, Sioyek,
  Catppuccin, Cherry Studio, Lucide, Niri, Retain PDF, and Typst.

Update a reference with `git submodule update --remote --depth 1 <path>`, then review and commit its
gitlink.

## gpui-component Policy

- Keep `main` as a fast-forward-only mirror of `upstream/main`.
- Maintain Glyphst changes on `glyphst/main` and merge `upstream/main` into it regularly.
- Keep composable widgets, styling workarounds, and application-specific keybindings in Glyphst.
- Change the fork only when a feature requires gpui-component private internals or must alter a
  component's internal event or render lifecycle.

## Development Constraint

Do not build Nix derivations unless explicitly requested.
