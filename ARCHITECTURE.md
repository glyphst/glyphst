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
| `pdf`, `pdf-gpu` | MuPDF integration and the standalone engine's GPUI adapter |

Private modules split implementation without introducing additional state
owners. New-tab and workspace entities own their task handles; document GPU
state owns device invalidation, cancellation, preparation, and cache lifetimes.
See [workspace](crates/workspace/src) and
[document GPU coordination](crates/document-ui/src/document_view/gpu_render).

The separate `glyphst-pdf` repository owns a runtime-neutral engine: core types,
a validated semantic scene, a private Hayro frontend, and CPU/WGPU renderers.
The [GPUI adapter](crates/pdf-gpu/src/lib.rs) shares the host device and queue,
uses its external-texture API, and keeps GPUI types outside the engine.
Frontend preflight, compilation, and renderer preflight must succeed before a
native page replaces MuPDF. A page never mixes engines or fallback raster islands.

Native page bounds, links and destinations use cropped/rotated physical points
(1/72 inch), matching MuPDF's reader geometry. The engine applies leaf-page
UserUnit before supplying those coordinates; the host must not scale them again.
Unsupported unit geometry follows the same whole-page fallback contract.

## Presentation and task lifetime

Native preparation and rendering carry cancellation and device generations.
A replacement device invalidates retained device resources. Superseded work
cannot publish over a newer request; the last coherent surface remains visible
until a valid replacement is ready. Incomplete refinement stays private, and
publication swaps a completed surface atomically.

Surface slots protect displayed and in-flight textures from overwrite. Visible
pages take priority over speculative preparation, within the existing worker,
cache, and surface budgets. Scale-changing frames render retained geometry at
the current camera; fixed-scale pan caches retain their exact device density.
Camera identity includes the page, serial, transform, clipped region, and target
geometry. Interaction diagnostics must describe actual execution for every
visible page, including neighbors entering the viewport.

When visible demand exceeds the prepared vector scale, refinement keeps the
required camera scale separate from preferred zoom headroom. Settled pages
reserve the same bounded headroom as increasing cameras. A headroom preparation
that exceeds a resource limit may retry once at the required scale; cancellation
and semantic failures do not retry. Worker reuse compares its target against
actual camera demand, and generation checks still govern page replacement.

The engine performs an explicit final-color conversion. The adapter preserves
sRGB versus ordinary UNORM texture-view encoding when GPUI samples cached
surfaces. Native interactive presentation does not read page pixels back to the
CPU or upload them through `RenderImage`.

## Library persistence

[Library storage](crates/library/src/library/storage.rs) has one current-format
initialization path for production and tests. `open_for_test` supplies a test
root; it does not select another schema or run legacy migrations. Unsupported
schema metadata is rejected without modification. Table names, serialization
keys, schema metadata, managed-library layout, and current-format decode
fallbacks are compatibility contracts.

## Native compiled-page cache

Each native document retains at most eight compiled pages and the engine's
`max_cpu_cache_bytes` budget, 512 MiB by default. Admission evicts LRU entries
until both limits hold. An oversized page bypasses caching without evicting
useful entries. Concurrent insertion of the same page reuses the cached entry.

The engine computes retained-size estimates off the UI path before the cache
lock is taken. Estimates include owned capacities, nested scenes/resources,
reader metadata, and conservative B-tree overhead. HUD snapshots read cached
counters. These estimates bound cache ownership, not process RSS or pages held
by active callers. Prepared resources and presentation surfaces have separate
limits.

`COMPILED_PAGE_CACHE_VERSION` in the engine is the semantic version authority.
The host derives `compiled-v{version}` from it. Old versions are not read into a
new namespace; old directories are left untouched. Records bind source, options,
and page identity, validate checksums, and revalidate decoded scene structure.
Checksums detect corruption, not malicious local writers. Encrypted documents
never read or write compiled pages on disk. The disk writer is bounded, idle,
and best effort.

Layer visibility is compiled into the cache identity. Layer-hidden links retain
annotation bounds for MuPDF geometry mapping but have empty hit regions and
inert actions. Appearance-only layers do not disable a visible annotation's link.

## Dependency maintenance

[Cargo.toml](Cargo.toml) and [Cargo.lock](Cargo.lock) identify published Zed,
GPUI Kit, and PDF revisions. Cargo and Nix use [rust-toolchain.toml](rust-toolchain.toml).
Sibling checkouts are optional; local overrides are described in
[README](README.md#working-on-maintained-dependencies).

For the maintained GPUI Kit fork, `main` is a fast-forward-only mirror of
`upstream/main`; `glyphst/main` carries Glyphst patches and merges upstream.
Keep composable widgets, styling workarounds, and application keybindings here.
Change Kit when private internals or its event/render lifecycle require it.
Preserve `gpui_component`, `gpui_component_assets`, and `gpui_fps` aliases;
`gpui_component_assets` resolves to `gpui-kit-assets`.

Keep GPUI revisions aligned across consumers. Publish Zed first, then Kit/PDF,
then update Glyphst's pins. Exact renderer/compiler/math pins and major library
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
