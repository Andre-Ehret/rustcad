# RustCAD — Working State

_Last updated: 2026-07-15_

A snapshot of where the project stands, how it is put together, and what is
deliberately left for later. For the full design rationale see
[`TECH_SPEC.md`](TECH_SPEC.md); for a user-facing overview see
[`README.md`](README.md).

## Status at a glance

- **MVP complete** — all six milestones from `TECH_SPEC.md` are implemented.
- **Tests: 36 passing** across the workspace (7 in `rustcad-core`,
  8 in `rustcad-geom`, 21 in `rustcad-sketch`), 0 failing.
- **Clippy: clean** with `-D warnings` (only a future-incompat note remains,
  and it comes from transitive deps `nom`/`quick-xml`, not our code).
- **Git**: on `main`, working tree clean. History was squashed into
  `ff0c01e Initial MVP Setup` on top of `d7baf86 Initial commit`.
- **No GitHub remote** is configured yet. A CI workflow is prepared and will
  run on the first push.

## Toolchain & environment

- **Rust** ≥ 1.85 (2021 edition). Verified building with `cargo 1.96.0`.
- Dependency versions are **pinned** because the geometry-kernel API is still
  moving — see `Cargo.toml` and the pitfalls section below. Key pins:
  `truck-modeling =0.6.0`, `truck-meshalgo =0.4.0`, `truck-polymesh =0.6.0`,
  `eframe`/`egui`/`egui-wgpu` `0.34.3` (wgpu 29 underneath), `glam 0.33`,
  `slotmap 1.1`, `nalgebra 0.35`, `ron 0.10`.
- `[profile.dev.package."*"] opt-level = 2` — dependencies are optimized even
  in dev builds, otherwise tessellation is sluggish.

### Build & run

```bash
cargo run -p rustcad-app        # launch the desktop app
cargo test --workspace          # run all tests
cargo clippy --workspace --all-targets -- -D warnings
```

> Note: on this machine `cargo` may not be on the shell `PATH`. If `cargo` is
> not found, run `export PATH="$HOME/.cargo/bin:$PATH"` first.

## Architecture

```
rustcad/
├── crates/
│   ├── rustcad-core/     # Document, feature tree, rebuild engine, .rcad persistence
│   ├── rustcad-sketch/   # 2D sketch, constraint solver, profile detection
│   ├── rustcad-geom/     # truck wrapper: extrude/revolve, tessellation, STL
│   └── rustcad-app/      # eframe app: viewport (wgpu), sketch editor, UI
└── TECH_SPEC.md          # full technical specification
```

`rustcad-core` and `rustcad-sketch` are **headless** (no GUI dependencies) and
fully testable with `cargo test`. All `truck` types stay encapsulated inside
`rustcad-geom`.

Approximate source sizes (LoC): `app.rs` 862, `sketch_mode.rs` 541,
`renderer.rs` 510, `core/lib.rs` 526, `geom/lib.rs` 478, `solver.rs` 458,
`sketch/lib.rs` 360, `profiles.rs` 306, `constraint.rs` 270,
`core/persist.rs` 145, `camera.rs` 95, `main.rs` 30.

## What works today (per milestone)

### M1 — Workspace & viewport shell
Multi-crate workspace, native eframe/egui window, wgpu viewport with an
orbit camera, grid, and axis gizmo.

### M2 — Sketching
Sketch on the standard planes XY / XZ / YZ. Lines (with chain mode) and
circles, endpoint snapping, selection, and delete. The sketch overlay is
drawn through the **egui painter in screen space** (a deliberate MVP
simplification of the spec's dedicated wgpu pipeline). Camera locks to the
sketch plane via `SketchSession::view_proj` (builds a look-at from the plane
axes u, v, n with u×v = n; the XZ plane uses n = −Y for a front view).

### M3 — Constraint solver
Constraints in `constraint.rs` are lowered to scalar equations over variable
IDs (residual + analytic Jacobian as closure callbacks). The solver in
`solver.rs` runs Levenberg–Marquardt via SVD with damped singular values
(σ/(σ²+λ)) → minimum-norm = **least-motion** behavior. Supported: coincident,
horizontal, vertical, parallel, perpendicular, distance, radius, equal-length.
Live point dragging uses `solve_drag` with a soft value constraint (weight
0.25); on non-convergence the best approximation is still written back for
good drag UX. Degrees-of-freedom are shown in the UI.

### M4 — Solids
Profile detection in `profiles.rs` (simple cycles, degree-2 check, union-find
over coincidence). `rustcad-geom` provides `extrude` and `revolve`
(Profile2D / Plane / Axis2D → truck). `tessellate` runs per B-rep face and
attaches `face_ids` per vertex. Face **picking** uses an offscreen pass into an
R32Uint texture (pick id = body<<16 | face, +1; 0 = background), read back via
`map_async` + `device.poll(PollType::wait_indefinitely())`.

### M5 — Parametric rebuild
`rustcad-core::Document` holds the feature history. `rebuild(doc, from,
&mut RebuildState)` caches entries before `from` (prefix validation over
feature IDs, otherwise a full rebuild). Changing a parameter rebuilds from the
affected feature down; errors (e.g. an open sketch) mark the feature **red**
instead of crashing. Sketches are re-editable (double-click in the tree);
`SketchSession.editing` carries the feature id on re-edit, and the tree /
properties panels are hidden while sketching to prevent deleting the sketch
being edited.

Documented MVP deviations from spec §4: `f64` parameters instead of a
`ParamTable`, no `ExtrudeDirection` (sign), standard planes only.

### M6 — Persistence & export
- `.rcad` = RON with `format_version: 1`
  (`rustcad_core::save_document` / `load_document`); slotmap-serde preserves
  key references across the round-trip. Geometry is rebuilt on load.
- **STL** binary export with face normals; `TriMesh::merge` for
  multi-body export.
- **Undo** = a stack of document clones (max 32) in the app. Drags push
  exactly one snapshot at drag start (`drag_started || !dragging` heuristic).
  **No redo** (deliberate).

## Controls

| Action | Input |
|---|---|
| Orbit | drag mouse (left/right) |
| Pan | drag middle mouse button |
| Zoom | scroll wheel |
| Fit view | `F` |
| Pick face | click (3D mode) |
| Undo | `⌘Z` / `Ctrl+Z` |
| Sketch: draw / select | click (Shift: multi-select) |
| Sketch: drag point | drag (solver runs live) |
| Sketch: cancel tool | `Esc` |
| Sketch: delete entity | `Del` / `Backspace` |

Typical flow: pick a sketch plane → draw a profile → add constraints →
**✔ Done** → **Extrude** / **Revolve** → adjust parameters in the properties
panel → save or export as STL.

## Known API pitfalls (verified, pinned in the workspace)

- **truck-modeling =0.6.0, truck-meshalgo =0.4.0**: cube via
  `builder::tsweep` ×3; `solid.triangulation(tol).to_polygon()`; edges via
  `edge_iter()` — dedup by `edge.id()` is required, it yields edges multiple
  times.
- **eframe/egui 0.34.3**: `App::update` is deprecated → use
  `fn ui(&mut self, ui: &mut egui::Ui, ...)` with
  `Panel::top(...)` / `CentralPanel::show_inside(ui, ...)`. `TopBottomPanel`
  is deprecated.
- **wgpu 29 (via egui-wgpu 0.34)**: `multiview` → `multiview_mask:
  Option<NonZeroU32>` (also on `RenderPassDescriptor`); `push_constant_ranges`
  → `immediate_size: u32`; `bind_group_layouts: &[Option<&Layout>]`;
  `depth_write_enabled` / `depth_compare` are now `Option`.
  **`DepthBiasState` is forbidden with `LineList` topology** (validation
  panic) → apply the line depth bias in the vertex shader
  (`clip_pos.z -= 2e-4 * w`).
- **Depth format**: `NativeOptions { depth_buffer: 32 }` makes the egui pass
  use `Depth32Float`; custom pipelines must match exactly (see the
  `DEPTH_FORMAT` constant in `renderer.rs`).

## Not in the MVP (v2 candidates)

Boolean operations, fillets/chamfers, assemblies, STEP import/export, nested
profiles (holes), named parameters with expressions, sketching on body faces,
circular arcs, and a constraint-delete UI.

## Suggested next steps

1. Set up the GitHub remote and push so CI runs for the first time.
2. Pick the first v2 feature (booleans and fillets are the most requested
   modeling gaps).
3. Consider redo support and a proper `ParamTable` with expressions, both
   already scoped in the spec.
