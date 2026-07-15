//! Projektformat `.rcad`: RON-Serialisierung des Dokuments
//! (TECH_SPEC §8). Geometrie wird nicht gespeichert — beim Laden
//! wird das Modell per [`crate::rebuild`] neu aufgebaut.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Document;

/// Aktuelle Version des Dateiformats.
///
/// * `1` — MVP (Features, Skizzen, Constraints).
/// * `2` — Bemaßungen (`Dimension`) in den Skizzen; abwärtskompatibel
///   lesbar, da das `dimensions`-Feld `#[serde(default)]` ist.
pub const FORMAT_VERSION: u32 = 2;

/// Fehler beim Speichern/Laden von `.rcad`-Dateien.
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    /// Dateisystem-Fehler.
    #[error("E/A-Fehler: {0}")]
    Io(#[from] std::io::Error),
    /// Serialisierung fehlgeschlagen.
    #[error("Serialisierung fehlgeschlagen: {0}")]
    Serialize(#[from] ron::Error),
    /// Datei ist kein gültiges RON / passt nicht zum Datenmodell.
    #[error("Datei nicht lesbar: {0}")]
    Deserialize(#[from] ron::error::SpannedError),
    /// Datei stammt aus einer neueren RustCAD-Version.
    #[error("Dateiformat-Version {0} wird nicht unterstützt (max. {FORMAT_VERSION})")]
    UnsupportedVersion(u32),
}

#[derive(Serialize, Deserialize)]
struct DocumentFile {
    format_version: u32,
    document: Document,
}

/// Speichert das Dokument als menschenlesbares RON.
pub fn save_document(doc: &Document, path: &Path) -> Result<(), PersistError> {
    let file = DocumentFile {
        format_version: FORMAT_VERSION,
        document: doc.clone(),
    };
    let text = ron::ser::to_string_pretty(&file, ron::ser::PrettyConfig::default())?;
    std::fs::write(path, text)?;
    Ok(())
}

/// Lädt ein Dokument; der Aufrufer stößt danach den Rebuild an.
pub fn load_document(path: &Path) -> Result<Document, PersistError> {
    let text = std::fs::read_to_string(path)?;
    let file: DocumentFile = ron::from_str(&text)?;
    if file.format_version > FORMAT_VERSION {
        return Err(PersistError::UnsupportedVersion(file.format_version));
    }
    Ok(file.document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{rebuild, RebuildState, RevolveAxis, SketchPlane};
    use rustcad_geom::TriMesh;
    use rustcad_sketch::Sketch;

    fn mesh_stats(meshes: &[(crate::FeatureId, &TriMesh)]) -> Vec<(usize, usize, f64)> {
        meshes
            .iter()
            .map(|(_, m)| {
                let volume: f64 = m
                    .indices
                    .chunks_exact(3)
                    .map(|t| {
                        let p = |i: u32| {
                            let v = m.positions[i as usize];
                            [v[0] as f64, v[1] as f64, v[2] as f64]
                        };
                        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                        (a[0] * (b[1] * c[2] - b[2] * c[1])
                            + a[1] * (b[2] * c[0] - b[0] * c[2])
                            + a[2] * (b[0] * c[1] - b[1] * c[0]))
                            / 6.0
                    })
                    .sum();
                (m.positions.len(), m.indices.len(), volume)
            })
            .collect()
    }

    /// Roundtrip-Test aus TECH_SPEC §10/M6: save → load → rebuild →
    /// identische Mesh-Statistik.
    #[test]
    fn rcad_roundtrip_preserves_mesh_statistics() {
        let mut doc = Document::new();
        let mut sketch = Sketch::new();
        let pts = [
            sketch.add_point([1.0, 0.0]),
            sketch.add_point([4.0, 0.0]),
            sketch.add_point([4.0, 2.0]),
            sketch.add_point([1.0, 2.0]),
        ];
        for i in 0..4 {
            sketch.add_line(pts[i], pts[(i + 1) % 4]);
        }
        let mut circle_sketch = Sketch::new();
        let center = circle_sketch.add_point([0.0, 0.0]);
        circle_sketch.add_circle(center, 1.5);

        let s1 = doc.add_sketch(SketchPlane::XY, sketch);
        doc.add_extrude(s1, 0, 3.0);
        let s2 = doc.add_sketch(SketchPlane::XZ, circle_sketch);
        doc.add_revolve(s1, 0, RevolveAxis::V, std::f64::consts::TAU);
        doc.add_extrude(s2, 0, 1.0);

        let mut state = RebuildState::default();
        rebuild(&doc, 0, &mut state);
        let stats_before = mesh_stats(&state.bodies());
        assert_eq!(stats_before.len(), 3);

        let path =
            std::env::temp_dir().join(format!("rustcad-roundtrip-{}.rcad", std::process::id()));
        save_document(&doc, &path).expect("save");
        let loaded = load_document(&path).expect("load");
        let _ = std::fs::remove_file(&path);

        let mut state_after = RebuildState::default();
        rebuild(&loaded, 0, &mut state_after);
        let stats_after = mesh_stats(&state_after.bodies());

        assert_eq!(stats_before, stats_after);
    }

    /// Roundtrip-Test für Bemaßungen: save → load → Bemaßungen inkl.
    /// Offsets identisch (Akzeptanzkriterium Issue 1).
    #[test]
    fn rcad_roundtrip_preserves_dimensions_and_offsets() {
        use rustcad_sketch::DimensionTarget;

        let mut doc = Document::new();
        let mut sketch = Sketch::new();
        let p1 = sketch.add_point([0.0, 0.0]);
        let p2 = sketch.add_point([4.0, 0.0]);
        let p3 = sketch.add_point([4.0, 2.0]);
        let p4 = sketch.add_point([0.0, 2.0]);
        for (a, b) in [(p1, p2), (p2, p3), (p3, p4), (p4, p1)] {
            sketch.add_line(a, b);
        }
        let center = sketch.add_point([2.0, 1.0]);
        let circle = sketch.add_circle(center, 0.5);

        sketch
            .add_dimension(DimensionTarget::Linear(p1, p2), 4.0, [0.0, -1.5])
            .expect("linear dimension");
        sketch
            .add_dimension(DimensionTarget::Diameter(circle), 1.0, [0.7, 0.7])
            .expect("diameter dimension");

        let before: Vec<_> = sketch.dimensions().map(|(_, d)| *d).collect();
        assert_eq!(before.len(), 2);

        doc.add_sketch(SketchPlane::XY, sketch);

        let path =
            std::env::temp_dir().join(format!("rustcad-dims-{}.rcad", std::process::id()));
        save_document(&doc, &path).expect("save");
        let loaded = load_document(&path).expect("load");
        let _ = std::fs::remove_file(&path);

        let (_, feature) = loaded.sketch_features()[0];
        let after: Vec<_> = feature.sketch.dimensions().map(|(_, d)| *d).collect();

        assert_eq!(before, after);
    }

    #[test]
    fn newer_format_version_is_rejected() {
        let path =
            std::env::temp_dir().join(format!("rustcad-version-{}.rcad", std::process::id()));
        let file = DocumentFile {
            format_version: 99,
            document: Document::new(),
        };
        std::fs::write(&path, ron::to_string(&file).expect("ron")).expect("write");
        let result = load_document(&path);
        let _ = std::fs::remove_file(&path);
        assert!(matches!(result, Err(PersistError::UnsupportedVersion(99))));
    }
}
