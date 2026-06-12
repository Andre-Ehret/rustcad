//! Datenmodell, Feature-Baum und Rebuild-Engine von RustCAD
//! (TECH_SPEC §4). Headless, keine GUI-Abhängigkeiten.
//!
//! Bewusste MVP-Vereinfachungen: Feature-Parameter sind direkte
//! `f64`-Werte (die benannte `ParamTable` mit Ausdrücken folgt später);
//! Extrude kennt nur die Normalenrichtung (Vorzeichen der Tiefe statt
//! `ExtrudeDirection`); Skizzierebenen sind die drei Standardebenen
//! (planare Flächen von Bodies folgen mit dem Face-Referenzmodell).

#![warn(missing_docs)]

mod persist;

pub use persist::{load_document, save_document, PersistError, FORMAT_VERSION};

use rustcad_geom::TriMesh;
use rustcad_sketch::Sketch;
use serde::{Deserialize, Serialize};
use slotmap::{new_key_type, SlotMap};

new_key_type! {
    /// Stabile, generationsbasierte ID eines Features.
    pub struct FeatureId;
}

/// Standard-Skizzierebene (durch den Ursprung).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SketchPlane {
    /// XY-Ebene, Blick von +Z.
    XY,
    /// XZ-Ebene, Blick von −Y (Frontansicht).
    XZ,
    /// YZ-Ebene, Blick von +X.
    YZ,
}

impl SketchPlane {
    /// Alle Standardebenen.
    pub const ALL: [SketchPlane; 3] = [SketchPlane::XY, SketchPlane::XZ, SketchPlane::YZ];

    /// Anzeigename.
    pub fn label(self) -> &'static str {
        match self {
            SketchPlane::XY => "XY",
            SketchPlane::XZ => "XZ",
            SketchPlane::YZ => "YZ",
        }
    }

    /// Ebenen-Achsen `(u, v, normal)` in Weltkoordinaten, rechtshändig
    /// (u × v = n).
    pub fn axes(self) -> ([f64; 3], [f64; 3], [f64; 3]) {
        match self {
            SketchPlane::XY => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
            SketchPlane::XZ => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
            SketchPlane::YZ => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        }
    }

    fn to_geom(self) -> rustcad_geom::Plane {
        let (u, v, _) = self.axes();
        rustcad_geom::Plane {
            origin: [0.0; 3],
            u,
            v,
        }
    }
}

/// Skizzen-Feature: Ebene + 2D-Skizze.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SketchFeature {
    /// Skizzierebene.
    pub plane: SketchPlane,
    /// Die Skizze (Entities, Constraints, Variablen).
    pub sketch: Sketch,
}

/// Extrusions-Feature; negatives `distance` extrudiert entgegen der
/// Ebenen-Normalen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtrudeFeature {
    /// Referenz auf das Skizzen-Feature.
    pub sketch: FeatureId,
    /// Index in die geschlossenen Profile der Skizze (ProfileSelection).
    pub profile: usize,
    /// Extrusionstiefe (parametrisch editierbar).
    pub distance: f64,
}

/// Rotationsachse eines Revolve, durch den Skizzen-Ursprung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RevolveAxis {
    /// u-Achse der Skizzierebene.
    U,
    /// v-Achse der Skizzierebene.
    V,
}

/// Rotations-Feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevolveFeature {
    /// Referenz auf das Skizzen-Feature.
    pub sketch: FeatureId,
    /// Index in die geschlossenen Profile der Skizze.
    pub profile: usize,
    /// Rotationsachse.
    pub axis: RevolveAxis,
    /// Winkel im Bogenmaß (≥ 2π = volle Rotation).
    pub angle: f64,
}

/// Ein Feature der Modellhistorie.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Feature {
    /// 2D-Skizze auf einer Ebene.
    Sketch(SketchFeature),
    /// Extrusion eines Skizzenprofils.
    Extrude(ExtrudeFeature),
    /// Rotation eines Skizzenprofils.
    Revolve(RevolveFeature),
}

impl Feature {
    /// Anzeigename des Feature-Typs.
    pub fn type_label(&self) -> &'static str {
        match self {
            Feature::Sketch(_) => "Skizze",
            Feature::Extrude(_) => "Extrude",
            Feature::Revolve(_) => "Revolve",
        }
    }

    /// Referenziertes Skizzen-Feature (falls vorhanden).
    pub fn sketch_ref(&self) -> Option<FeatureId> {
        match self {
            Feature::Sketch(_) => None,
            Feature::Extrude(e) => Some(e.sketch),
            Feature::Revolve(r) => Some(r.sketch),
        }
    }
}

/// Das Dokument: geordnete Feature-Historie.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Document {
    features: Vec<FeatureId>,
    store: SlotMap<FeatureId, Feature>,
}

impl Document {
    /// Leeres Dokument.
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, feature: Feature) -> FeatureId {
        let id = self.store.insert(feature);
        self.features.push(id);
        id
    }

    /// Hängt ein Skizzen-Feature an die Historie an.
    pub fn add_sketch(&mut self, plane: SketchPlane, sketch: Sketch) -> FeatureId {
        self.push(Feature::Sketch(SketchFeature { plane, sketch }))
    }

    /// Hängt ein Extrude-Feature an die Historie an.
    pub fn add_extrude(&mut self, sketch: FeatureId, profile: usize, distance: f64) -> FeatureId {
        self.push(Feature::Extrude(ExtrudeFeature {
            sketch,
            profile,
            distance,
        }))
    }

    /// Hängt ein Revolve-Feature an die Historie an.
    pub fn add_revolve(
        &mut self,
        sketch: FeatureId,
        profile: usize,
        axis: RevolveAxis,
        angle: f64,
    ) -> FeatureId {
        self.push(Feature::Revolve(RevolveFeature {
            sketch,
            profile,
            axis,
            angle,
        }))
    }

    /// Features in Historien-Reihenfolge.
    pub fn features(&self) -> impl Iterator<Item = (FeatureId, &Feature)> {
        self.features.iter().map(|&id| (id, &self.store[id]))
    }

    /// Ein einzelnes Feature.
    pub fn feature(&self, id: FeatureId) -> Option<&Feature> {
        self.store.get(id)
    }

    /// Mutabler Zugriff auf ein Feature. Danach [`rebuild`] ab
    /// [`Document::index_of`] aufrufen.
    pub fn feature_mut(&mut self, id: FeatureId) -> Option<&mut Feature> {
        self.store.get_mut(id)
    }

    /// Position eines Features in der Historie.
    pub fn index_of(&self, id: FeatureId) -> Option<usize> {
        self.features.iter().position(|&f| f == id)
    }

    /// Anzahl der Features.
    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// `true`, wenn die Historie leer ist.
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    /// Alle Skizzen-Features in Historien-Reihenfolge.
    pub fn sketch_features(&self) -> Vec<(FeatureId, &SketchFeature)> {
        self.features()
            .filter_map(|(id, f)| match f {
                Feature::Sketch(s) => Some((id, s)),
                _ => None,
            })
            .collect()
    }

    /// Entfernt ein Feature samt abhängiger Features (Extrude/Revolve,
    /// die eine entfernte Skizze referenzieren). Liefert den kleinsten
    /// betroffenen Historien-Index als Startpunkt für den Rebuild.
    pub fn remove(&mut self, id: FeatureId) -> usize {
        let Some(first_index) = self.index_of(id) else {
            return self.features.len();
        };
        let mut removed = vec![id];
        let dependents: Vec<FeatureId> = self
            .features()
            .filter(|(_, f)| f.sketch_ref() == Some(id))
            .map(|(fid, _)| fid)
            .collect();
        removed.extend(dependents);

        for fid in &removed {
            self.store.remove(*fid);
        }
        self.features.retain(|f| !removed.contains(f));
        first_index.min(self.features.len())
    }
}

/// Status eines Features nach dem Rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureStatus {
    /// Erfolgreich aufgebaut.
    Ok,
    /// Aufbau fehlgeschlagen (z. B. Skizze nicht mehr geschlossen);
    /// das Feature wird im Baum rot markiert statt zu crashen.
    Failed(String),
}

/// Rebuild-Ergebnis eines Features.
#[derive(Debug, Clone)]
pub struct RebuildEntry {
    /// Zugehöriges Feature.
    pub feature: FeatureId,
    /// Status.
    pub status: FeatureStatus,
    /// Tessellierter Body (nur bei Extrude/Revolve mit Status Ok).
    pub body: Option<TriMesh>,
}

/// Gecachter Rebuild-Zustand, parallel zur Feature-Historie.
#[derive(Debug, Clone, Default)]
pub struct RebuildState {
    /// Einträge in Historien-Reihenfolge.
    pub entries: Vec<RebuildEntry>,
}

impl RebuildState {
    /// Status eines Features, falls es einen Rebuild-Eintrag hat.
    pub fn status_of(&self, id: FeatureId) -> Option<&FeatureStatus> {
        self.entries
            .iter()
            .find(|e| e.feature == id)
            .map(|e| &e.status)
    }

    /// Alle Bodies mit erzeugendem Feature, in Historien-Reihenfolge.
    pub fn bodies(&self) -> Vec<(FeatureId, &TriMesh)> {
        self.entries
            .iter()
            .filter_map(|e| e.body.as_ref().map(|b| (e.feature, b)))
            .collect()
    }
}

/// Mesh-Toleranz für den Rebuild.
const TESSELLATION_TOLERANCE: f64 = 0.005;

/// Sequenzieller Rebuild ab Historien-Index `from` (TECH_SPEC §4.2).
/// Einträge davor werden aus `state` wiederverwendet, sofern sie noch
/// zur Historie passen — sonst wird komplett neu aufgebaut.
pub fn rebuild(doc: &Document, from: usize, state: &mut RebuildState) {
    let mut from = from.min(doc.features.len());
    let prefix_valid = from <= state.entries.len()
        && doc.features[..from]
            .iter()
            .zip(&state.entries)
            .all(|(&id, entry)| entry.feature == id);
    if !prefix_valid {
        from = 0;
    }
    state.entries.truncate(from);

    for &id in &doc.features[from..] {
        state.entries.push(compute_entry(doc, id));
    }
}

fn compute_entry(doc: &Document, id: FeatureId) -> RebuildEntry {
    let result = match &doc.store[id] {
        Feature::Sketch(_) => Ok(None),
        Feature::Extrude(e) => sketch_profile(doc, e.sketch, e.profile).and_then(|(p, plane)| {
            rustcad_geom::extrude(&p, &plane, e.distance)
                .map(Some)
                .map_err(|err| err.to_string())
        }),
        Feature::Revolve(r) => sketch_profile(doc, r.sketch, r.profile).and_then(|(p, plane)| {
            let dir = match r.axis {
                RevolveAxis::U => [1.0, 0.0],
                RevolveAxis::V => [0.0, 1.0],
            };
            let axis = rustcad_geom::Axis2D {
                origin: [0.0, 0.0],
                dir,
            };
            rustcad_geom::revolve(&p, &plane, &axis, r.angle)
                .map(Some)
                .map_err(|err| err.to_string())
        }),
    };

    match result {
        Ok(solid) => RebuildEntry {
            feature: id,
            status: FeatureStatus::Ok,
            body: solid.map(|s| rustcad_geom::tessellate(&s, TESSELLATION_TOLERANCE)),
        },
        Err(message) => RebuildEntry {
            feature: id,
            status: FeatureStatus::Failed(message),
            body: None,
        },
    }
}

/// Löst die Profil-Referenz eines Features auf.
fn sketch_profile(
    doc: &Document,
    sketch: FeatureId,
    profile: usize,
) -> Result<(rustcad_geom::Profile2D, rustcad_geom::Plane), String> {
    let Some(Feature::Sketch(sf)) = doc.feature(sketch) else {
        return Err("referenzierte Skizze existiert nicht mehr".into());
    };
    let profiles = sf.sketch.find_profiles();
    if profiles.is_empty() {
        return Err("Skizze enthält keine geschlossene Schleife".into());
    }
    let p = profiles.get(profile).ok_or(format!(
        "Profil {} existiert nicht ({} vorhanden)",
        profile + 1,
        profiles.len()
    ))?;
    let geom_profile = match p {
        rustcad_sketch::Profile::Polygon(points) => {
            rustcad_geom::Profile2D::Polygon(points.clone())
        }
        rustcad_sketch::Profile::Circle { center, radius } => rustcad_geom::Profile2D::Circle {
            center: *center,
            radius: *radius,
        },
    };
    Ok((geom_profile, sf.plane.to_geom()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh_volume(mesh: &TriMesh) -> f64 {
        let p = |i: u32| {
            let v = mesh.positions[i as usize];
            [v[0] as f64, v[1] as f64, v[2] as f64]
        };
        mesh.indices
            .chunks_exact(3)
            .map(|t| {
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                let cross = [
                    b[1] * c[2] - b[2] * c[1],
                    b[2] * c[0] - b[0] * c[2],
                    b[0] * c[1] - b[1] * c[0],
                ];
                (a[0] * cross[0] + a[1] * cross[1] + a[2] * cross[2]) / 6.0
            })
            .sum::<f64>()
            .abs()
    }

    fn square_sketch(size: f64) -> Sketch {
        let mut s = Sketch::new();
        let pts = [
            s.add_point([0.0, 0.0]),
            s.add_point([size, 0.0]),
            s.add_point([size, size]),
            s.add_point([0.0, size]),
        ];
        for i in 0..4 {
            s.add_line(pts[i], pts[(i + 1) % 4]);
        }
        s
    }

    #[test]
    fn extrude_rebuild_produces_expected_volume() {
        let mut doc = Document::new();
        let sketch = doc.add_sketch(SketchPlane::XY, square_sketch(10.0));
        doc.add_extrude(sketch, 0, 5.0);

        let mut state = RebuildState::default();
        rebuild(&doc, 0, &mut state);

        assert_eq!(state.entries.len(), 2);
        assert!(state.entries.iter().all(|e| e.status == FeatureStatus::Ok));
        let bodies = state.bodies();
        assert_eq!(bodies.len(), 1);
        assert!((mesh_volume(bodies[0].1) - 500.0).abs() < 1e-6);
    }

    #[test]
    fn parameter_edit_rebuilds_from_feature() {
        let mut doc = Document::new();
        let sketch = doc.add_sketch(SketchPlane::XY, square_sketch(10.0));
        let extrude = doc.add_extrude(sketch, 0, 5.0);

        let mut state = RebuildState::default();
        rebuild(&doc, 0, &mut state);

        if let Some(Feature::Extrude(e)) = doc.feature_mut(extrude) {
            e.distance = 10.0;
        }
        let from = doc.index_of(extrude).unwrap();
        rebuild(&doc, from, &mut state);

        let bodies = state.bodies();
        assert!((mesh_volume(bodies[0].1) - 1000.0).abs() < 1e-6);
    }

    #[test]
    fn open_sketch_marks_feature_failed() {
        let mut doc = Document::new();
        let mut s = Sketch::new();
        let a = s.add_point([0.0, 0.0]);
        let b = s.add_point([10.0, 0.0]);
        s.add_line(a, b); // offene Skizze
        let sketch = doc.add_sketch(SketchPlane::XY, s);
        let extrude = doc.add_extrude(sketch, 0, 5.0);

        let mut state = RebuildState::default();
        rebuild(&doc, 0, &mut state);

        match state.status_of(extrude) {
            Some(FeatureStatus::Failed(msg)) => {
                assert!(msg.contains("geschlossene"), "Meldung war: {msg}")
            }
            other => panic!("Failed erwartet, war {other:?}"),
        }
        assert!(state.bodies().is_empty());
    }

    #[test]
    fn partial_rebuild_keeps_earlier_entries() {
        let mut doc = Document::new();
        let s1 = doc.add_sketch(SketchPlane::XY, square_sketch(10.0));
        doc.add_extrude(s1, 0, 5.0);
        let s2 = doc.add_sketch(SketchPlane::XZ, square_sketch(2.0));
        let e2 = doc.add_extrude(s2, 0, 1.0);

        let mut state = RebuildState::default();
        rebuild(&doc, 0, &mut state);
        assert_eq!(state.bodies().len(), 2);

        if let Some(Feature::Extrude(e)) = doc.feature_mut(e2) {
            e.distance = 3.0;
        }
        rebuild(&doc, doc.index_of(e2).unwrap(), &mut state);

        let bodies = state.bodies();
        assert_eq!(bodies.len(), 2);
        assert!((mesh_volume(bodies[0].1) - 500.0).abs() < 1e-6);
        assert!((mesh_volume(bodies[1].1) - 12.0).abs() < 1e-6);
    }

    #[test]
    fn removing_sketch_removes_dependents() {
        let mut doc = Document::new();
        let sketch = doc.add_sketch(SketchPlane::XY, square_sketch(10.0));
        doc.add_extrude(sketch, 0, 5.0);

        let from = doc.remove(sketch);
        assert_eq!(from, 0);
        assert!(doc.is_empty());

        // Rebuild mit ungültig gewordenem Cache fällt auf 0 zurück
        let mut state = RebuildState::default();
        rebuild(&doc, 0, &mut state);
        assert!(state.entries.is_empty());
    }
}
