# Glyphst

Glyphst is a workspace for desktop applications built with Rust,
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), and
[gpui-component](https://github.com/longbridge/gpui-component).

## Applications

### Glyphst Reader

A PDF and EPUB reader with tabbed reading, search, document navigation, annotations, AI chat,
translation workflows, themes, and configurable keybindings.

### Glyphst Files

An early GPUI file application shell. Its initial window demonstrates the shared Glyphst UI
foundation without loading Reader settings or persisted state.

## Development

Rust stable and the native libraries required by GPUI are needed. Glyphst Reader additionally
requires MuPDF. On Linux, the Nix development shell provides the required libraries and tools.

```sh
git submodule update --init dependencies/gpui-component
nix develop
cargo run -p glyphst-reader --release -- /path/to/document.pdf
cargo run -p glyphst-files
```

Without Nix, install the platform packages listed in
[CI](.github/workflows/ci.yml) before running Cargo.

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
