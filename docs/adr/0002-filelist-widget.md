# ADR-0002: FileList is a custom-drawn widget

Status: accepted · 2026-11-14

## Context

Panels must show 100k+ entries without stutter, support Brief (column-major, Far-style), Full and
Custom column modes, box-drawing frames in the Far Blue theme, per-row highlight colours/icons, a
cursor that differs between active and inactive panels, and full keyboard/mouse control (F02, F30, F32).
`GtkColumnView`/`GtkListView` allocate a widget per visible row, are row-major only, and make
monospace-grid rendering and retro frames awkward.

## Decision

- `ofm-app/src/widgets/file_list/` implements `FileList`, a `gtk::Widget` subclass that draws only the
  visible rows in `snapshot()` using cached `pango::Layout`s, and implements `gtk::Scrollable`.
- It reads from a `PanelModel` (`ofm-core`) through a small `FileListModel` adapter; the model's
  `ModelChange` (`Reset`, `Spliced`) drives incremental invalidation. No `GListModel` for rows.
- Layout, hit-testing (row/column from x,y) and column geometry are pure functions in the widget
  module so they are unit-testable without a display.
- Keyboard handling is NOT in the widget: the window-level `CommandDispatcher` (capture phase)
  executes commands (`panel.cursor_down`, ...) on the controller. The widget handles mouse only
  (click, double-click, Shift/Ctrl click, right-click signal, wheel, drag source).
- Accessibility: role `list`, accessible label updated with the entry under the cursor (selection
  count in the description).
- Colours come from the theme's render palette (ADR-0005), never hard-coded.

## Alternatives considered

- `GtkColumnView` + `GListModel`: simple, but per-row widgets, no column-major layout, hard to get the
  Far look and 100k-row smoothness.
- `GtkGridView` for Brief: wrong fill order, no frames.
- Canvas (Cairo `DrawingArea`): works but loses GTK4 scene-graph caching and accessibility hooks.

## Consequences

- We own scrolling, selection painting, hit-testing and a11y of the central widget (more code, full control).
- A debug frame-time log is part of the widget (T2.1 acceptance: 100k entries smooth).
- Theming is by palette, so the widget stays independent of CSS.
