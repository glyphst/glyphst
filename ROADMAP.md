# Next work

A short ordered shortlist for tasks without a more specific user direction.
Select one bounded step, verify its current premise, and finish its acceptance
checks. Update affected entries when evidence changes; remove completed work
rather than appending a progress history. Evidence below was reviewed on
2026-10-11; each entry identifies its relevant checkpoint or evidence.

Use [architecture](ARCHITECTURE.md) for boundaries and
[verification](README.md#verification) for commands.

## 1. Workspace layout on GPUI Kit dock

**Owner:** `workspace` (panes, tabs, docks, session), with GPUI Kit fork patches
where the dock lacks a capability.

**Current evidence:** A component audit on 2026-10-11 found the rest of the UI
layer already built on GPUI Kit: `ui::picker` renders through Kit `List` and
`Input`, `ui::scrollbar` wraps Kit `Scrollbar`, `ui::markdown` wraps
`TextView`, prompts and dialogs use Kit dialogs, and the settings window uses
Kit setting items. `activity`, `sticky_items` and `entry_selection` are
application behaviour without a Kit counterpart. The remaining duplication is
the workspace's own layout system: `pane.rs` (3.9k lines), `workspace/docks.rs`,
`synchronized_tabs.rs`, `workspace/synchronized_connections.rs` and
`workspace/session.rs`, with about 4.7k lines of tests. GPUI Kit 0.7.1 ships a
docking system (`gpui_base::dock`: `DockArea`, `LayoutTree`, `TabGroup`,
`Panel`, `PanelRegistry`, drag with edge drop zones, zoom, `dump`/`load`) and a
replaceable presentation (`DockAreaRenderer`).

| Workspace feature | Kit dock mapping |
| --- | --- |
| Pane with document, new-tab and loading tabs | `TabGroup`; each tab view implements `Panel`/`PanelView` |
| Nested splits, ratios, reset | Center `LayoutTree` splits, `split_at`, `set_split_sizes` |
| Tab drag between panes and edge split | Built-in panel drag, `move_panel`, edge drop zones |
| Tab title, suffix, menu | `Panel::title`, `title_suffix`, `dropdown_menu` |
| Left document sidebar, right AI dock | `DockPlacement::Left`/`Right`, `toggle_dock`, `set_dock_size` |
| Active outline, dimmed inactive panes, synchronized connection lines | An application `DockAreaRenderer` that also records tab bounds |
| Session restore (`ReaderPaneTree`) | `DockAreaState` via `dump`/`load` and a `PanelRegistry`; format may change |

**Gaps to resolve:** dock open/close is not animated (the current docks
tween their width); zen mode must hide tab bars through the renderer; restored
documents open asynchronously, so a panel must exist before its document loads.

**Next step:** Implement in the order side docks, tab panels, center splits,
synchronized tabs and connections on the renderer, then session state, keeping
`script/check.sh quick` green at each step and rewriting pane tests against the
Kit dock.

**Done when:** `pane.rs`, `PaneGroup` and the custom dock code are gone, every
listed feature works on the Kit dock, session restore round-trips, and the
user's smoke test of splits, tab drag, docks and synchronized tabs passes.

## 2. Long-document coordinate precision

**Owner:** `document-ui` layout and viewport geometry.

**Current evidence:** [Page geometry](crates/document-ui/src/document_view/geometry.rs)
subtracts document scroll coordinates in f64, but page offsets and other layout
values still originate as f32, which is not proven pixel-stable at maximum zoom
deep in a long document.

**Next step:** Add a focused numerical reproduction with large cumulative page
offsets, then keep layout origins at sufficient precision until viewport-relative
conversion. Preserve rotation, fractional device origin, and camera identity.

**Done when:** Equivalent near-origin and distant-page crops agree at extreme
zoom, with round-trip scroll/zoom restoration passing. Do not infer correctness
solely from ordinary first-page rendering.

## 3. Dependency audit follow-up

**Owner:** Workspace dependency maintenance.

**Current evidence:** [.cargo/audit.toml](.cargo/audit.toml) and the
[architecture rationale](ARCHITECTURE.md#dependency-audit) retain two quick-xml
exceptions. The reviewed audit also reports unmaintained `bincode`, `cgmath`,
`instant`, `paste`, `rustls-pemfile`, `rustybuzz`, `ttf-parser`, and `yaml-rust`,
plus cgmath advisory RUSTSEC-2026-0197. A refreshed audit on 2026-10-11, after
the dependency upgrades, reports the same nine allowed warnings; quick-xml
0.38.4 is still reachable only through Typst 0.15.1, the latest release.

**Next step:** Refresh advisories and identify the currently reachable dependency
paths. When the pinned Typst chain supports a fixed quick-xml version (at least
0.41 for the reviewed advisories), make the smallest compatible update and
remove the corresponding exceptions. Handle remaining warnings by owner/path.

**Done when:** An audit without the removed exceptions confirms the fix, locked
workspace checks pass, and any remaining warnings retain accurate dispositions.
Do not add ignores merely to obtain a clean summary.
