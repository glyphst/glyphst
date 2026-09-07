# Next work

A short ordered shortlist for tasks without a more specific user direction.
Select one bounded step, verify its current premise, and finish its acceptance
checks. Update affected entries when evidence changes; remove completed work
rather than appending a progress history. Evidence below was reviewed on
2026-09-07; each entry identifies its relevant checkpoint or evidence.

The [PDF roadmap](https://github.com/glyphst/glyphst-pdf/blob/main/ROADMAP.md)
(`../glyphst-pdf/ROADMAP.md` locally) owns engine semantics and rendering work.
Use [architecture](ARCHITECTURE.md) for boundaries and
[verification](README.md#verification) for commands. Artifact paths below are
relative to the external `../pdf-test-suite`; they are optional local evidence,
not build inputs. Reproduce the relevant check if that evidence is unavailable.

## 1. Reader presentation and timing

**Owner:** `document-ui`, `pdf-gpu`, with engine coordination as needed.

**Current evidence:** The image-mipmap comparison uses Glyphst `1462afd`
with engine `9bfef43` versus `541da46`. All common-profile non-timing gates
pass across three repetitions, but both builds have timing failures. A declared
three-pair book-zoom check finds median interval p95 rising from 8.318 to
10.509 ms, exceeding its 5% regression bound. The affected pages contain no
images and GPU work counters match; the timing cause remains unresolved. A
fresh baseline rebuild reproduces the frozen binary hash. The subsequent
content-diagnostics engine update passes all common-profile gates in three
repetitions, as does its frozen baseline. Those runs do not reproduce the older
failure, but do not identify its cause or repeat the declared paired experiment.
The widget-admission integration check again fails timing in eight of nine
repetitions on both the candidate and its frozen predecessor. Both pass every
non-timing gate; these runs reproduce the symptom without isolating its cause.

Evidence: `reports/image-mipmap-area-2026-09-07/verification.md`, and
`reports/reader-interaction/run-1788783459558` and `run-1788785428149`. The newer
passing comparison is `reports/content-diagnostics-2026-09-07/verification.md`.
The current failing comparison is `reports/widget-appearances-2026-09-08/verification.md`.
Dense CAD performance also remains open; its evidence is in
`reports/inactive-stroke-draws-2026-09-06`.

**Next step:** Resolve the book-zoom timing regression with matched release
builds, separating deadline scheduling, GPUI draw, final-color/presentation,
and engine execution costs. Then return to a dense zoom/pan workload. Keep
the common profile as the integration regression gate.

**Done when:** The selected workload meets its declared improvement criterion
with matching release baselines, unchanged visual/camera/allocation checks,
and all repetitions reported. Full interactive acceptance still requires the
existing absolute and paired timing gates; an improvement alone does not close
this item.

## 2. Long-document coordinate precision

**Owner:** `document-ui` layout and viewport geometry.

**Current evidence:** [Page geometry](crates/document-ui/src/document_view/geometry.rs)
subtracts document scroll coordinates in f64, but page offsets and other layout
values still originate as f32. The engine's f64 camera transform does not by
itself prove pixel-stable 8,192x navigation deep in a long document.

**Next step:** Add a focused numerical reproduction with large cumulative page
offsets, then keep layout origins at sufficient precision until viewport-relative
conversion. Preserve rotation, fractional device origin, and camera identity.

**Done when:** Equivalent near-origin and distant-page crops agree at extreme
zoom, with round-trip scroll/zoom restoration and relevant interaction checks
passing. Do not infer correctness solely from ordinary first-page rendering.

## 3. Dependency audit follow-up

**Owner:** Workspace dependency maintenance.

**Current evidence:** [.cargo/audit.toml](.cargo/audit.toml) and the
[architecture rationale](ARCHITECTURE.md#dependency-audit) retain two quick-xml
exceptions. The reviewed audit also reports unmaintained `bincode`, `cgmath`,
`instant`, `paste`, `rustls-pemfile`, `rustybuzz`, `ttf-parser`, and `yaml-rust`,
plus cgmath advisory RUSTSEC-2026-0197. These are dated observations, not a
fresh audit result.

**Next step:** Refresh advisories and identify the currently reachable dependency
paths. When the pinned Typst chain supports a fixed quick-xml version (at least
0.41 for the reviewed advisories), make the smallest compatible update and
remove the corresponding exceptions. Handle remaining warnings by owner/path.

**Done when:** An audit without the removed exceptions confirms the fix, locked
workspace checks pass, and any remaining warnings retain accurate dispositions.
Do not add ignores merely to obtain a clean summary.
