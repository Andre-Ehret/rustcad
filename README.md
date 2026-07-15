# RustCAD

Ein parametrischer 3D-CAD-Prototyp in Rust: Sketch-basiertes Modellieren mit
Constraint-Solver, Feature-Historie und parametrischem Rebuild — gerendert in
einer nativen Desktop-App (egui + wgpu) auf dem B-Rep-Kernel
[truck](https://github.com/ricosjp/truck).

## Features (MVP)

- **Skizzieren** auf den Standardebenen XY/XZ/YZ: Linien (mit Kettenmodus)
  und Kreise, Endpunkt-Snapping, Selektion und Löschen
- **Constraint-Solver** (Newton/Levenberg-Marquardt, analytische Jacobi):
  koinzident, horizontal, vertikal, parallel, senkrecht, Abstand, Radius,
  gleich lang — mit Least-Motion-Verhalten, Live-Drag von Punkten und
  Freiheitsgrad-Anzeige
- **Solids**: Profil-Erkennung geschlossener Schleifen, **Extrude** und
  **Revolve** über den truck-Kernel
- **Parametrik**: Feature-Baum mit Historie; Parameter ändern baut das Modell
  ab dem betroffenen Feature neu auf, Fehler (z. B. offene Skizze) markieren
  das Feature rot statt zu crashen; Skizzen nachträglich editierbar
  (Doppelklick im Baum)
- **Viewport**: Orbit-Kamera, Grid + Achsenkreuz, Kanten-Rendering,
  Face-Picking per ID-Buffer
- **Persistenz & Export**: eigenes Projektformat `.rcad` (menschenlesbares
  RON, versioniert — Geometrie wird beim Laden neu aufgebaut), binärer
  **STL-Export**, Snapshot-Undo (⌘/Strg+Z)

## Bauen & Starten

Benötigt wird ein aktuelles stabiles Rust (≥ 1.85, [rustup](https://rustup.rs)).

App starten (Debug — schneller Build, ausreichend dank optimierter Abhängigkeiten):

```bash
cargo run -p rustcad-app
```

Für flüssigere Tessellierung/Rendering als Release-Build:

```bash
cargo run --release -p rustcad-app
```

## Bedienung

| Aktion | Eingabe |
|---|---|
| Orbit | Maus ziehen (links/rechts) |
| Pan | mittlere Maustaste ziehen |
| Zoom | Scrollrad |
| Ansicht einpassen | `F` |
| Fläche picken | Klick (3D-Modus) |
| Undo | `⌘Z` / `Strg+Z` |
| Skizze: zeichnen/auswählen | Klick (Shift: Mehrfachauswahl) |
| Skizze: Punkt ziehen | Drag (Solver läuft live) |
| Skizze: Werkzeug abbrechen | `Esc` |
| Skizze: Entity löschen | `Entf` / `Backspace` |

Typischer Ablauf: Skizzierebene wählen → Profil zeichnen → Constraints setzen
→ **✔ Fertig** → **Extrude**/**Revolve** → Parameter im Eigenschaften-Panel
anpassen → speichern oder als STL exportieren.

## Architektur

```
rustcad/
├── crates/
│   ├── rustcad-core/     # Document, Feature-Baum, Rebuild-Engine, .rcad-Persistenz
│   ├── rustcad-sketch/   # 2D-Skizze, Constraint-Solver, Profil-Erkennung
│   ├── rustcad-geom/     # truck-Wrapper: Extrude/Revolve, Tessellierung, STL
│   └── rustcad-app/      # eframe-App: Viewport (wgpu), Skizzen-Editor, UI
└── TECH_SPEC.md          # vollständige technische Spezifikation
```

`rustcad-core` und `rustcad-sketch` sind headless (keine GUI-Abhängigkeiten)
und vollständig per `cargo test` testbar. Sämtliche truck-Typen bleiben in
`rustcad-geom` gekapselt.

## Tests

```bash
cargo test --workspace
```

Enthält u. a. Golden-Tests über Mesh-Volumenintegrale (Extrude 10×10×5 → 500,
Revolve gegen die Pappus-Regel), Jacobi-Matrizen gegen numerische Differenz,
Property-Tests für die Solver-Konvergenz (proptest) und den
`.rcad`-Roundtrip-Test (save → load → rebuild → identische Mesh-Statistik).

## Nicht im MVP (v2-Kandidaten)

Boolesche Operationen, Fillets/Chamfers, Baugruppen, STEP-Import/-Export,
verschachtelte Profile (Löcher), benannte Parameter mit Ausdrücken,
Skizzieren auf Body-Flächen, Kreisbögen.

## Lizenz

MIT oder Apache-2.0, nach Wahl.
