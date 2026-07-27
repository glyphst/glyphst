# Project Information

A desktop PDF viewer built using GPUI.

## Reference Projects

Reference projects are tracked as shallow Git submodules in `references/`:

- `zed` (Editor; The source of GPUI)
- `zathura` (A PDF viewer)
- `sioyek` (A PDF viewer)
- `catppuccin` (Theme/Color palette)
- `cherry-studio` (An AI chat application)
- `lucide` (Icon)
- `niri` (Wayland compositor)
- `retain-pdf` (Quality PDF translation project)
- `typst` (Typst)

Initialize a reference on demand with `git submodule update --init --depth 1 <reference-path>`.
Update it to the tip of its configured branch with
`git submodule update --remote --depth 1 <reference-path>`, review the change, and commit the new
gitlink.

## gpui-component Fork

The maintained fork is a full-history submodule at `dependencies/gpui-component`. Cargo uses its
`crates/ui` and `crates/assets` packages as path dependencies, so the parent repository's gitlink
is the exact development version pin. Initialize it without `--depth`:

```sh
git submodule update --init dependencies/gpui-component
```

The checkout uses:

- `origin`: `hs-gitea:glyphst/gpui-component.git`
- `upstream`: `https://github.com/longbridge/gpui-component.git`

Normal submodule initialization checks out the pinned commit in detached-HEAD state. Before editing
the fork, run `git -C dependencies/gpui-component switch glyphst/main`. If a fresh clone does not
have the `upstream` remote, add it with:

```sh
git -C dependencies/gpui-component remote add upstream \
  https://github.com/longbridge/gpui-component.git
```

Keep `main` as a clean, fast-forward-only mirror of `upstream/main`. Maintain Glyphst-only changes
on the long-lived `glyphst/main` branch and merge `upstream/main` into it regularly.

Nix flakes omit Git submodule contents by default. To avoid fetching every large reference
submodule, `flake.lock` has a separate `gpuiComponent` source whose revision must match the gitlink.
CI checks that equality.

Keep composable widgets, styling workarounds, and application-specific keybindings in this
repository. Change the fork only when the feature requires access to gpui-component private
internals or must alter a component's internal event/render lifecycle.

For a routine sync:

1. Fetch `upstream`.
2. Fast-forward `main` to `upstream/main` and push `main` to `origin`.
3. Merge `upstream/main` into `glyphst/main`.
4. Run gpui-component's focused library tests when the merge changes fork-owned code or has
   conflicts.
5. Push `glyphst/main` and update the `dependencies/gpui-component` gitlink in Glyphst.
6. Run `nix flake update gpuiComponent` so the Nix source pin matches the gitlink. Regenerate
   `Cargo.lock` only when the component's package metadata or dependencies changed.
7. Test Glyphst and commit the updated gitlink and lock-file pin together.

## Dev Note

Don't build Nix derivations unless asked explicitly.
