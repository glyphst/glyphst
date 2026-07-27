# Glyphst Reader

Glyphst Reader is a desktop PDF and EPUB reader built with Rust and
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

## Highlights

- Tabbed reading with search, thumbnails, outlines, bookmarks, highlights, and notes.
- A persistent document library with reading-position history.
- AI chat with document, page, selection, and region context.
- Text and full-document translation workflows.
- Customizable themes and keybindings.

## Development

Rust stable and the native libraries required by GPUI and MuPDF are needed. On Linux, the Nix
development shell provides the required libraries and tools.

```sh
git submodule update --init dependencies/gpui-component
nix develop
cargo run -p glyphst-reader --release -- /path/to/document.pdf
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

## Profiling

Start Tracy and run:

```sh
ZTRACING=1 cargo run -p glyphst-reader --features tracy --release
```

## License

Glyphst Reader is licensed under [AGPL-3.0-only](LICENSE).
