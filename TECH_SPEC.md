# RustCAD — Tech-Spezifikation MVP

> Arbeitsdokument für Claude Code. Ziel: ein parametrischer 3D-CAD-Prototyp in Rust
> mit nativer Desktop-GUI, Sketch-basiertem Modellieren und Feature-Historie.

---

## 1. Produktvision (MVP-Scope)

Der Nutzer kann:

1. Eine Skizzierebene wählen (XY, XZ, YZ oder planare Fläche eines Körpers)
2. In der Skizze Linien, Kreise und Bögen zeichnen
3. Geometrische Constraints setzen (koinzident, horizontal, vertikal, parallel, senkrecht, Abstand, Radius, gleich)
4. Die Skizze per **Extrude** oder **Revolve** in einen Volumenkörper überführen
5. Den Feature-Baum sehen und Parameter (z. B. Extrusionstiefe, Maße) nachträglich ändern → Modell wird neu aufgebaut (parametrischer Rebuild)
6. Das Modell als **STL** exportieren und als eigenes Projektformat speichern/laden

**Explizit NICHT im MVP:** Boolesche Operationen zwischen Bodies, Fillets/Chamfers, Baugruppen (Assemblies), STEP-Import, technische Zeichnungsableitung, Undo/Redo über Snapshots hinaus.

---

## 2. Architektur-Übersicht

```
┌─────────────────────────────────────────────────────┐
│                      app (eframe/egui)               │
│  UI: Feature-Baum, Toolbar, Parameter-Panels         │
├──────────────────────┬──────────────────────────────┤
│   viewport (wgpu)    │        sketch_editor          │
│  3D-Rendering,       │  2D-Modus auf Ebene,          │
│  Orbit-Kamera,       │  Zeichenwerkzeuge,            │
│  Picking             │  Constraint-Anzeige           │
├──────────────────────┴──────────────────────────────┤
│                     core (lib)                       │
│  ┌────────────┐ ┌──────────────┐ ┌───────────────┐  │
│  │ document   │ │ constraints  │ │ features      │  │
│  │ Feature-   │ │ 2D-Solver    │ │ Extrude,      │  │
│  │ Baum, IDs  │ │ (Newton/LM)  │ │ Revolve       │  │
│  └────────────┘ └──────────────┘ └───────────────┘  │
├──────────────────────────────────────────────────────┤
│              geometry kernel: truck                  │
│  B-Rep (Solid, Shell, Face, Edge), Tessellierung     │
└──────────────────────────────────────────────────────┘
```

### Workspace-Struktur (Cargo)

```
rustcad/
├── Cargo.toml              # [workspace]
├── crates/
│   ├── rustcad-core/       # Datenmodell, Feature-Baum, Rebuild-Engine
│   ├── rustcad-sketch/     # 2D-Skizze + Constraint-Solver
│   ├── rustcad-geom/       # Wrapper um truck: Extrude/Revolve, Tessellierung, STL
│   └── rustcad-app/        # eframe-App: Viewport, UI, Interaktion
└── TECH_SPEC.md
```

Regel: `rustcad-core` und `rustcad-sketch` haben **keine** GUI-Abhängigkeiten und sind vollständig headless testbar.

---

## 3. Technologie-Entscheidungen

| Bereich | Wahl | Begründung |
|---|---|---|
| Geometrie-Kernel | **`truck`** (truck-modeling, truck-meshalgo, truck-polymesh) | Einziger ernstzunehmender B-Rep-Kernel in pure Rust. Bietet Extrude/Sweep/Revolve (`builder::tsweep`, `builder::rsweep`), NURBS-Flächen und Tessellierung. Einen eigenen Kernel zu schreiben ist kein MVP. |
| GUI-Framework | **`eframe`/`egui`** | Immediate-Mode, schnell zu iterieren, exzellente wgpu-Integration über `egui-wgpu`. Panels/Trees/Inputs out of the box. |
| Rendering | **`wgpu`** (custom render pass im egui-Callback) | Volle Kontrolle über 3D-Viewport (Mesh-Rendering, Kanten, Picking). egui rendert UI darüber. |
| Lineare Algebra | **`nalgebra`** | Solver braucht dynamische Matrizen + Least-Squares. (`glam` optional nur im Renderer; an der Schnittstelle konvertieren.) |
| Entity-IDs | **`slotmap`** | Stabile, generationsbasierte IDs für Skizzen-Entities, Features, Faces. |
| Serialisierung | **`serde` + `ron`** | Eigenes Projektformat `.rcad` als RON — menschenlesbar, gut diffbar. |
| Fehler | **`thiserror`** (lib) / **`anyhow`** (app) | Standard. |

**Hinweis an Claude Code:** Vor Nutzung von `truck`-APIs die aktuelle Doku auf docs.rs prüfen — die API ist noch in Bewegung. Versionen im Workspace pinnen.

---

## 4. Datenmodell (rustcad-core)

### 4.1 Dokument & Feature-Baum

```rust
pub struct Document {
    pub features: Vec<FeatureId>,          // geordnete Historie
    pub store: SlotMap<FeatureId, Feature>,
    pub params: ParamTable,                // benannte Parameter (später: Ausdrücke)
}

pub enum Feature {
    Sketch(SketchFeature),     // referenziert Ebene + Sketch-Daten
    Extrude(ExtrudeFeature),   // referenziert Sketch-Profil + Tiefe
    Revolve(RevolveFeature),   // referenziert Sketch-Profil + Achse + Winkel
}

pub struct ExtrudeFeature {
    pub sketch: FeatureId,
    pub profile: ProfileSelection,   // welche geschlossene Region der Skizze
    pub distance: Param<f64>,        // parametrisch!
    pub direction: ExtrudeDirection, // Normal / beidseitig
}
```

### 4.2 Rebuild-Engine

- Der Rebuild läuft **sequenziell** über die Feature-Liste (MVP: keine Dependency-Graph-Optimierung).
- Jedes Feature erzeugt/modifiziert einen `BodyState` (truck `Solid` + Tessellierung als Cache).
- Ändert der Nutzer einen Parameter, wird ab dem betroffenen Feature neu aufgebaut.
- Fehler im Rebuild (z. B. Skizze nicht mehr geschlossen) markieren das Feature als `Failed` im Baum, statt zu crashen.

```rust
pub fn rebuild(doc: &Document, from: usize) -> RebuildResult { ... }
```

---

## 5. Skizze & Constraint-Solver (rustcad-sketch)

### 5.1 Skizzen-Entities

```rust
pub enum SketchEntity {
    Point(PointId),                          // freier Punkt
    Line { p1: PointId, p2: PointId },
    Circle { center: PointId, radius: VarId },
    Arc { center: PointId, start: PointId, end: PointId },
}
```

Alle Koordinaten und Radien sind Einträge in einem flachen Variablenvektor `Vec<f64>` — der Solver arbeitet direkt darauf.

### 5.2 Constraints (MVP-Satz)

| Constraint | Gleichung(en) |
|---|---|
| Coincident(p, q) | `px − qx = 0`, `py − qy = 0` |
| Horizontal(line) | `p1y − p2y = 0` |
| Vertical(line) | `p1x − p2x = 0` |
| Parallel(l1, l2) | Kreuzprodukt der Richtungsvektoren = 0 |
| Perpendicular(l1, l2) | Skalarprodukt = 0 |
| Distance(p, q, d) | `‖p − q‖² − d² = 0` |
| Radius(circle, r) | `radius − r = 0` |
| Equal(l1, l2) | `‖l1‖² − ‖l2‖² = 0` |

### 5.3 Solver-Strategie

- **Newton-Raphson mit Levenberg-Marquardt-Dämpfung** auf dem Residuenvektor `F(x) = 0`.
- Jacobi-Matrix **analytisch** pro Constraint-Typ (keine numerische Differenzierung — zu instabil).
- Unterbestimmte Systeme sind der Normalfall (Skizze nicht voll bestimmt): kleinste-Quadrate-Lösung via `nalgebra` (SVD / pseudo-inverse), Startwerte = aktuelle Positionen → Solver bewegt Geometrie minimal ("least motion").
- Konvergenzkriterium: `‖F‖∞ < 1e-9`, max. 50 Iterationen, sonst `SolveResult::DidNotConverge`.
- DOF-Anzeige: `freie Variablen − Rang(J)` als "n Freiheitsgrade" in der UI.

**Alternative, falls der eigene Solver zu lange dauert:** FFI-Bindings zu libslvs (SolveSpace-Solver). Erst evaluieren, wenn der eigene Solver bei Milestone 3 hakt — pure Rust ist bevorzugt.

### 5.4 Profil-Erkennung

Vor Extrude: geschlossene Schleifen in der Skizze finden (Graph aus Endpunkten, Kantenzyklen via Planar-Face-Traversal). MVP: nur einfache, nicht-verschachtelte Schleifen; eine äußere Schleife + innere Löcher in v2.

---

## 6. Geometrie-Layer (rustcad-geom)

Dünner Wrapper um `truck`, damit Core/UI nie direkt mit truck-Typen sprechen:

```rust
pub fn extrude(profile: &Profile2D, plane: &Plane, distance: f64) -> Result<Solid, GeomError>;
pub fn revolve(profile: &Profile2D, plane: &Plane, axis: &Axis2D, angle: f64) -> Result<Solid, GeomError>;
pub fn tessellate(solid: &Solid, tolerance: f64) -> TriMesh;   // Vertices, Normalen, Indices + Kanten-Polylinien
pub fn export_stl(mesh: &TriMesh, path: &Path) -> Result<(), GeomError>;
```

- Profil → truck `Wire` (Linien/Bögen als Edges) → `builder::try_attach_plane` → `tsweep`/`rsweep`.
- Tessellierung über `truck-meshalgo` mit Toleranz abhängig von Bounding-Box-Größe.
- Zusätzlich **Kanten-Polylinien** extrahieren für die Edge-Darstellung im Viewport.

---

## 7. App & Viewport (rustcad-app)

### 7.1 Rendering

- egui-Fenster mit zentralem `PaintCallback` → eigener wgpu-Renderpass.
- Pipeline 1: Flächen (flat shading + einfaches Lambert-Licht, eine Direktionale).
- Pipeline 2: Kanten als Linien (dunkler, leichter Depth-Bias).
- Pipeline 3: Skizzen-Overlay (Linien/Punkte in Bildschirmraum-Dicke).
- Orbit-Kamera: Rotation (rechte Maustaste/Drag), Pan (Mitte), Zoom (Scroll), `f` = fit view.

### 7.2 Picking

MVP: **ID-Buffer-Picking** — Offscreen-Pass rendert Entity-IDs als Farbwerte, Mausposition → readback eines Pixels. Robuster und einfacher als Ray-Casting gegen B-Rep.

### 7.3 UI-Layout

```
┌────────────┬─────────────────────────────┬──────────────┐
│ Feature-   │                             │ Eigenschaften│
│ Baum       │        3D-Viewport          │ (Parameter   │
│ (egui      │   (Sketch-Modus = Kamera    │  des selek-  │
│  Tree)     │    lockt auf Ebene)         │  tierten     │
│            │                             │  Features)   │
├────────────┴─────────────────────────────┴──────────────┤
│ Toolbar: [Sketch] [Linie] [Kreis] [Constraints…]         │
│          [Extrude] [Revolve] [Export STL]                │
└──────────────────────────────────────────────────────────┘
```

### 7.4 Interaktions-Zustandsmaschine

```rust
enum AppMode {
    Idle,                              // 3D-Navigation, Selektion
    SketchEdit { sketch: FeatureId, tool: SketchTool },
    FeatureDialog(PendingFeature),     // Extrude-Parameter etc.
}
```

Keine impliziten Modi — jeder Tool-Wechsel geht durch diese Enum. Escape bricht das aktuelle Tool ab.

---

## 8. Dateiformat & Export

- **`.rcad`** = RON-Serialisierung von `Document` (Feature-Baum + Skizzen + Parameter). Geometrie wird NICHT gespeichert — beim Laden wird rebuilt. Versionfeld `format_version: u32` von Anfang an.
- **STL-Export** (binär) aus der Tessellierung.
- v2-Kandidaten: STEP-Export via `truck-stepio`, OBJ.

---

## 9. Meilensteine (für Claude Code abarbeitbar)

### M1 — Fundament (Viewport)
- [ ] Cargo-Workspace, CI (fmt, clippy, test)
- [ ] eframe-App mit wgpu-PaintCallback
- [ ] Orbit-Kamera + Grid + Achsenkreuz
- [ ] Hartkodierter Würfel (truck → tessellate → render) als End-to-End-Beweis

### M2 — Skizzieren (ohne Constraints)
- [ ] Skizzierebene wählen (XY/XZ/YZ), Kamera-Lock auf Ebene
- [ ] Linien- und Kreis-Werkzeug, Punkt-Snapping auf Endpunkte
- [ ] Selektion + Löschen von Entities

### M3 — Constraint-Solver
- [ ] Variablenvektor, Residuen + analytische Jacobi für alle 8 Constraints
- [ ] LM-Solver mit Least-Motion-Verhalten
- [ ] Drag eines Punktes löst live (Drag-Position als temporärer Constraint)
- [ ] DOF-Anzeige
- [ ] **Property-Tests:** zufällige lösbare Systeme konvergieren; gelöste Systeme erfüllen alle Residuen < 1e-9

### M4 — Solids
- [ ] Profil-Erkennung (geschlossene Schleifen)
- [ ] Extrude + Revolve über truck
- [ ] Kanten-Rendering, Face-Picking via ID-Buffer

### M5 — Parametrik
- [ ] Feature-Baum-UI, Parameter editieren → Rebuild ab Feature
- [ ] Fehlerzustände im Baum (Skizze offen → Feature rot)
- [ ] Skizze nachträglich editieren (Doppelklick im Baum)

### M6 — Persistenz & Export
- [ ] `.rcad` speichern/laden (Roundtrip-Test: save → load → rebuild → identische Mesh-Statistik)
- [ ] STL-Export
- [ ] Fit-View, einfache Snapshot-basierte Undo (ein Stack von `Document`-Klonen)

---

## 10. Teststrategie

- `rustcad-sketch`: Unit-Tests pro Constraint (Residuum + Jacobi gegen numerische Differenz prüfen), Property-Tests mit `proptest` für Solver-Konvergenz.
- `rustcad-core`: Rebuild-Tests headless (Dokument programmatisch bauen, Mesh-Volumen/BBox asserten).
- `rustcad-geom`: Golden-Tests — Extrude eines 10×10-Quadrats um 5 → Volumen ≈ 500 (über Mesh-Volumenintegral).
- GUI bleibt im MVP ungetestet; Logik dafür konsequent in die Lib-Crates schieben.

## 11. Konventionen für Claude Code

- Rust Edition 2021+, `cargo fmt` + `clippy -D warnings` müssen grün sein.
- Keine `unwrap()` in Lib-Crates (nur in Tests und am App-Einstieg).
- Öffentliche APIs der Lib-Crates dokumentieren (`#![warn(missing_docs)]` in core/sketch/geom).
- Jede truck-Interaktion bleibt in `rustcad-geom` — Leak von truck-Typen in andere Crates ist ein Review-Fehler.
- Commits pro Meilenstein-Checkbox, kleine PR-große Schritte.
