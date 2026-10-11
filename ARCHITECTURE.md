# Architecture and dependency boundaries

## Ownership

| Owner | Responsibility |
| --- | --- |
| `reader`, `files` | Application entry points |
| `workspace` | Panes, tabs, home/library browsing, window commands |
| `document-ui` | Document views, interaction, and UI task lifetimes |
| `ui`, `actions`, `keymap` | Shared widgets and application input |
| `settings`, `settings-ui` | Settings data and its editor |
| `library` | Managed documents, annotations, reader sessions, persistence |
| `ai`, `ai-ui` | AI services and chat presentation |
| Translation crates | Translation domain, PDF production, service orchestration, UI |
| `pdf` | MuPDF integration: document opening, render sessions, display lists |

Private modules split implementation without introducing additional state
owners. New-tab and workspace entities own their task handles; document views
own their render requests, text indexing, and cancellation. See
[workspace](crates/workspace/src) and [document views](crates/document-ui/src/document_view).

MuPDF context clones share stylesheet settings across threads.
[Document opening](crates/pdf/src/document.rs) serializes configuration. PDF
opens preserve the user CSS allocation and enable document styles for rich-text
appearances, avoiding repeated writes when already enabled. EPUB stylesheet
and publisher-style settings still have process-wide ownership; concurrent
documents cannot rely on independent EPUB style policies.

## Presentation and task lifetime

PDF and EPUB pages are rasterized by MuPDF off the UI thread and presented as
`RenderImage`s. Render keys identify the document, page, source revision,
rotation, color transform, and physical size. The process-wide render cache
bounds raster bytes and display lists; keys for the current viewport are
protected from eviction while they are displayed or pending. Visible pages are
requested first and adjacent pages are prefetched within a pixel limit. Until
the exact raster is ready, the closest cached raster for the page remains
visible.

## Library persistence

[Library storage](crates/library/src/library/storage.rs) has one current-format
initialization path for production and tests. `open_for_test` supplies a test
root; it does not select another schema or run legacy migrations. Unsupported
schema metadata is rejected without modification. Table names, serialization
keys, schema metadata, managed-library layout, and current-format decode
fallbacks are compatibility contracts.

Glyphst carries no database migrations; there are no external users to
preserve data for. When redb reports a file from an older file format
(`UpgradeRequired`), the library and the translation cache rename it aside as
`<name>.redb<version>-format` (never deleting it) and start a fresh database.

## Dependency maintenance

[Cargo.toml](Cargo.toml) and [Cargo.lock](Cargo.lock) pin GPUI to an exact
crates.io `gpui-pre` snapshot and GPUI Kit to a published fork revision. Cargo and Nix use [rust-toolchain.toml](rust-toolchain.toml).
Sibling checkouts are optional; local overrides are described in
[README](README.md#working-on-maintained-dependencies).

For the maintained GPUI Kit fork, `main` is a fast-forward-only mirror of
`upstream/main`; `glyphst/main` is upstream plus a short, explained delta listed
in the fork's `AGENTS.md`. Prefer GPUI Kit components and upstream APIs; keep
composable widgets, styling workarounds, and application keybindings here, and
patch Kit only when it lacks a capability Glyphst needs. Preserve the
`gpui_component`, `gpui_component_assets`, `gpui_base` and `gpui_fps` aliases;
`gpui_component_assets` resolves to `gpui-kit-assets`.

The app's `gpui-pre` pin must equal the snapshot GPUI Kit requires; bump both
together. Publish Kit, then update Glyphst's pins. Exact renderer/compiler/math pins and major library
upgrades require compatibility work. Do not commit override-generated lockfile
changes.

`references/` contains shallow upstream reference submodules, including Zed;
these are not maintained forks or build inputs. To refresh an intended reference,
use `git submodule update --remote --depth 1 references/<name>` and review its
gitlink. Preserve copied-source and theme provenance independently of renames.

## Dependency audit

[The audit policy](.cargo/audit.toml) records the reviewed quick-xml exceptions
RUSTSEC-2026-0194 and RUSTSEC-2026-0195. At the September 7, 2026 checkpoint,
`quick-xml 0.38.4` remains through `citationberg 0.7.0 -> hayagriva 0.10.1 ->
typst-library 0.15.1`. This is runtime CSL parsing. The Typst file loader limits
project/package access but does not generally bound XML CPU or allocation work.

A configured audit pass does not mean the dependency graph is warning-free.
Exception removal and remaining warning paths are tracked in
[ROADMAP](ROADMAP.md#3-dependency-audit-follow-up); refresh the advisory database
before treating old results as current security evidence.
