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
- `gpui-component` (Third-party GPUI Components)
- `retain-pdf` (Quality PDF translation project)
- `typst` (Typst)

Initialize them with `git submodule update --init --depth 1`. Update a reference to the tip of its
configured branch with `git submodule update --remote --depth 1 <path>`, review the change, and
commit the new gitlink.

## gpui-component Fork

The maintained fork lives at `glyphst/gpui-component` on the private Gitea instance. Its local
checkout uses:

- `origin`: `hs-gitea:glyphst/gpui-component.git`
- `upstream`: `https://github.com/longbridge/gpui-component.git`

Keep `main` as a clean, fast-forward-only mirror of `upstream/main`. Maintain Glyphst-only changes
on the long-lived `glyphst/main` branch and merge `upstream/main` into it regularly. Glyphst pins
that branch by exact commit SHA.

Keep composable widgets, styling workarounds, and application-specific keybindings in this
repository. Change the fork only when the feature requires access to gpui-component private
internals or must alter a component's internal event/render lifecycle.

For a routine sync:

1. Fetch `upstream`.
2. Fast-forward `main` to `upstream/main` and push `main` to `origin`.
3. Merge `upstream/main` into `glyphst/main`.
4. Run gpui-component's focused library tests when the merge changes fork-owned code or has
   conflicts.
5. Push `glyphst/main`, update Glyphst's exact Cargo SHA, and test Glyphst.

## Dev Note

Don't build Nix derivations unless asked explicitly.
