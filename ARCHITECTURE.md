# Architecture and dependency boundaries

## Ownership

`reader` and `files` are application entry points. `workspace` owns panes, tabs,
home/library browsing, and window commands. `document-ui` owns document views and
their UI task lifetimes. Shared widgets live in `ui`; settings, the managed
library, AI, and translation retain their existing domain/service/UI boundaries.
No new crate, scheduler, persistence layer, or cross-application framework was
introduced by the maintenance refactor.

The large UI implementations are split into private modules:

| Owner | Implementation modules |
| --- | --- |
| `workspace/new_tab` | `catalog`, `selection`, `thumbnails`, `rendering`, `tests` |
| `workspace/workspace` | `session`, `synchronized_connections`, existing `ai`, `chrome`, `docks`, `render_debug`, `tests` |
| `document-ui/document_view/gpu_render` | `state`, `preparation`, `presentation`, `diagnostics`, `tests` |

These modules keep the same entities, captures, task handles, cancellation
tokens/generations, and ordering. They are implementation boundaries, not public
APIs or additional objects. New-tab and workspace state remain with their owners;
GPU state owns device invalidation, cancellation, and cache lifetimes.

`glyphst-pdf` is a separate, runtime-neutral engine: core types, validated scene
IR, a private Hayro frontend, the engine facade, and CPU/WGPU renderers. The
application's `pdf-gpu` crate bridges it to the maintained GPUI same-device
external-texture API. MuPDF remains in the application's `pdf` crate. Unsupported
native semantics require whole-page fallback; a refactor must not mix engines
within one page or relax renderer preflight.

## Library persistence

The current-format opener is the single database initialization path in
production and tests. `open_for_test` only supplies a test root; it does not
select a different schema or run legacy migrations. Obsolete migration code and
its migration-only DTOs are removed. Current table names, serialization keys,
schema metadata, managed-library layout, and decode fallbacks still used by the
current format remain unchanged. Unsupported schema metadata is rejected without
modifying it. No existing user database, attachment, or document is migrated or
deleted by this maintenance change.

## Native compiled-page cache

Each native document retains at most eight compiled pages and the engine's
`max_cpu_cache_bytes` budget (512 MiB by default). Admission evicts LRU entries
until both limits hold. A page larger than the byte budget is returned to the
caller without caching it or evicting useful entries. Concurrent insertion of
the same page reuses the existing entry without counting it twice.

The engine computes retained-size estimates once, off the UI path, before the
application takes the cache lock. Estimates include vector/string capacity,
nested owned scenes and resources, text/link/annotation data, and conservative
B-tree overhead. HUD/trace snapshots read cached byte counters. This bounds
estimated cache ownership, not process RSS or pages still held by active callers.
Prepared GPU resources and presentation surfaces retain their independent limits.

`COMPILED_PAGE_CACHE_VERSION` in the engine is the only semantic cache version
authority. The host derives `compiled-v2` from it; version 1 is not read through
the new namespace and old records fail the engine version check. Existing old
cache directories are left untouched. Checksums detect corruption, not malicious
local writers. Source/options/page bindings and scene validation remain required.
Encrypted documents never read or write compiled pages on disk. The existing
bounded, idle, best-effort disk writer is unchanged.

## Dependency maintenance

Cargo pins the published `glyphst/zed`, `glyphst/gpui-kit`, and `glyphst/glyphst-pdf`
revisions; sibling clones are optional development conveniences. GPUI Kit's
upstream repository was renamed, but `gpui-component` remains a real package.
The application keeps its existing Rust aliases; the assets alias now resolves
to `gpui-kit-assets`. There is no UI dependency submodule or separate Nix source
pin. Cargo and Nix use `rust-toolchain.toml` and the committed Cargo lockfile.

The maintained Kit branch keeps the existing input, Markdown/math, resize, and
diagnostic integrations. Its `main` branch only fast-forwards to upstream;
`glyphst/main` merges upstream and carries Glyphst patches. Publish dependency
commits before consumer pins. Reference submodules are shallow, read-only, and
unrelated to those maintained forks. Historical copied theme provenance is not
rewritten merely because a repository was renamed.

The September 2026 update refreshes compatible lockfile entries, Nix inputs, and
reference gitlinks, and moves Mermaid's direct `quick-xml` dependency to 0.42.
Exact renderer/compiler/math pins and major MuPDF, redb, or reqwest migrations
remain separate compatibility work, not automatic version bumps.

The audit still explicitly ignores [RUSTSEC-2026-0194](https://rustsec.org/advisories/RUSTSEC-2026-0194.html)
and [RUSTSEC-2026-0195](https://rustsec.org/advisories/RUSTSEC-2026-0195.html) for
`quick-xml 0.38.4` through `citationberg 0.7.0 -> hayagriva 0.10.1 -> typst-library
0.15.1`. This is runtime CSL parsing, not the old GPUI build-script explanation.
Typst can parse custom CSL bytes; Glyphst's file loader restricts project/package
access, but that is not a general CPU or allocation bound on XML parsing.
Do not describe the dependency graph as vulnerability-free. Remove the exceptions
when the pinned Typst chain supports a fixed XML version (at least 0.41), and
rerun the audit without the ignore configuration to verify that condition.
