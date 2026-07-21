# GPUI Template

A small Rust starter project for building desktop applications with
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).

## Use this template

1. Create a new repository from this template on GitHub.
2. Rename the package in `crates/app/Cargo.toml`.
3. Replace the starter UI in `crates/app/src/main.rs`.
4. Update this README with your application details.

After pushing this repository to GitHub, enable **Settings → General → Template
repository** so it can be used from the **Use this template** button.

## Prerequisites

- Rust stable with the 2024 edition.
- Linux GPUI system dependencies. The included `flake.nix` provides these.
- Optional: Nix for the development shell, which includes the Linux system
  dependencies and local tooling.
- Optional without Nix: [`prek`](https://github.com/j178/prek), `cargo-audit`,
  and the GPUI Linux system dependencies installed manually.

## Getting started

```sh
# Optional: enter the dev shell
nix develop

# Run the starter app
cargo run
```

## Profiling

Glyphst supports Zed-style CPU tracing with
[Tracy](https://github.com/wolfpld/tracy). Start the Tracy profiler, then run:

```sh
ZTRACING=1 cargo run -p glyphst --features tracy --release
```

The core PDF loading, text extraction, display-list creation, rendering, and
image rasterization paths are instrumented. Add more traced functions with:

```rust
#[ztracing::instrument(skip_all)]
fn function_to_profile() {}
```

Use a release build when profiling because debug builds have different
performance characteristics. Profiling instrumentation is a no-op unless the
`tracy` feature is enabled and `ZTRACING` is set.

## Development workflow

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo audit
```

## Pre-commit hooks

Install hooks once, preferably from inside `nix develop`:

```sh
prek install
```

The hooks run TOML checks, EOF cleanup, secret detection, Rust formatting,
Clippy, and `cargo audit`; see `prek.toml`.

## Project structure

```text
.
├── .cargo/audit.toml       cargo-audit policy
├── .github/workflows/ci.yml
├── Cargo.toml              Cargo workspace
├── crates/app              Starter GPUI application
├── flake.nix               Optional Nix development shell
└── prek.toml               Pre-commit hook configuration
```

## Credits
(TODO: add links)

- Zed editor
- Zathura
- Sioyek
- Retain-pdf
