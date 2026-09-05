# Glyphst

Glyphst is a workspace for desktop applications built with Rust,
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), and
[GPUI Kit](https://github.com/longbridge/gpui-kit) (through its `gpui-component` facade).

## Applications

### Glyphst Reader

A PDF and EPUB reader with tabbed reading, search, document navigation, annotations, AI chat,
translation workflows, themes, and configurable keybindings.

### Glyphst Files

A lightweight, read-only file navigator with Yazi-inspired parent, current-directory, and preview
panes. It supports Vim-style keyboard navigation and line-oriented UTF-8 text previews.

## Development

The Rust version in `rust-toolchain.toml` (1.97.1) and GPUI's native libraries are needed. Glyphst Reader additionally
requires MuPDF. On Linux, the Nix development shell provides the required libraries and tools.

```sh
nix develop
cargo run -p glyphst-reader --release -- /path/to/document.pdf
cargo run -p glyphst-files -- /path/to/start
```

Without Nix, install the platform packages listed in
[CI](.github/workflows/ci.yml) before running Cargo.

Normal builds use the published Git revisions in `Cargo.toml` and `Cargo.lock`;
no dependency submodule or sibling checkout is required. Nix uses the same Rust
toolchain file and Cargo lockfile. The read-only repositories in `references/`
are optional research material, not build inputs.

### Working on maintained dependencies

Keep independent checkouts beside this repository: `../gpui-kit` on `glyphst/main`,
`../glyphst-pdf` on `main`, and `../zed` on `glyphst/main`. The former
`dependencies/gpui-component` checkout has been replaced by `../gpui-kit`.
`references/zed` is a read-only upstream reference, not our GPUI fork.

For local development, put only the overrides you need in the ignored
`.cargo/config.local.toml`:

```toml
[patch."https://github.com/glyphst/gpui-kit"]
gpui-component = { path = "../gpui-kit/crates/component" }
gpui-kit-assets = { path = "../gpui-kit/crates/assets" }
gpui-fps = { path = "../gpui-kit/crates/fps" }

[patch."https://github.com/glyphst/glyphst-pdf"]
glyphst-pdf-engine = { path = "../glyphst-pdf/crates/engine" }
glyphst-pdf-render-cpu = { path = "../glyphst-pdf/crates/render-cpu" }
glyphst-pdf-render-wgpu = { path = "../glyphst-pdf/crates/render-wgpu" }

[patch."https://github.com/glyphst/zed"]
gpui = { path = "../zed/crates/gpui" }
gpui_platform = { path = "../zed/crates/gpui_platform" }
gpui_wgpu = { path = "../zed/crates/gpui_wgpu" }
gpui_macros = { path = "../zed/crates/gpui_macros" }
reqwest_client = { path = "../zed/crates/reqwest_client" }
ztracing = { path = "../zed/crates/ztracing" }
```

Use `cargo --config .cargo/config.local.toml check --workspace --all-targets --all-features`.
These overrides are opt-in; normal Cargo commands and CI do not load this file.
Do not commit override-generated lockfile changes. Publish Zed before GPUI Kit,
then publish GPUI Kit/PDF before updating this workspace's revisions. Omit the
local config, run `cargo metadata --format-version 1` to restore Git resolution,
and review the lockfile before the normal `--locked` checks.

See [architecture and dependency boundaries](ARCHITECTURE.md) for ownership,
persistence, cache limits, and the remaining dependency-audit exception.

To inspect the file-open animation, exit any running Reader and start a debug build with a minimum
loader duration in milliseconds:

```sh
GLYPHST_DEBUG_FILE_OPEN_MIN_MS=3000 cargo run -p glyphst-reader -- /path/to/document.pdf
```

Release builds ignore this variable.

Run the standard checks with:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo audit
```

Install the configured pre-commit hooks with `prek install`.

## Reader Profiling

Start Tracy and run:

```sh
ZTRACING=1 cargo run -p glyphst-reader --features tracy --release
```

## License

Glyphst is licensed under [AGPL-3.0-only](LICENSE).
