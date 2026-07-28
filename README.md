# Glyphst

Glyphst is a workspace for desktop applications built with Rust,
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), and
[gpui-component](https://github.com/longbridge/gpui-component).

## Applications

### Glyphst Reader

A PDF and EPUB reader with tabbed reading, search, document navigation, annotations, AI chat,
translation workflows, themes, and configurable keybindings.

### Glyphst Files

A lightweight, read-only file navigator with Yazi-inspired parent, current-directory, and preview
panes. It supports Vim-style keyboard navigation and line-oriented UTF-8 text previews.

## Development

Rust stable and the native libraries required by GPUI are needed. Glyphst Reader additionally
requires MuPDF. On Linux, the Nix development shell provides the required libraries and tools.

```sh
git submodule update --init dependencies/gpui-component
nix develop
cargo run -p glyphst-reader --release -- /path/to/document.pdf
cargo run -p glyphst-files -- /path/to/start
```

Without Nix, install the platform packages listed in
[CI](.github/workflows/ci.yml) before running Cargo.

To inspect the file-open animation, exit any running Reader and start a debug build with a minimum
loader duration in milliseconds:

```sh
GLYPHST_DEBUG_FILE_OPEN_MIN_MS=3000 cargo run -p glyphst-reader -- /path/to/document.pdf
```

Release builds ignore this variable.

Run the standard checks with:

```sh
cargo fmt --check
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
