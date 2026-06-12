//! Geometrie-Layer von RustCAD: dünner Wrapper um den `truck`-Kernel.
//!
//! Sämtliche truck-Typen bleiben in diesem Crate gekapselt — nach außen
//! gehen ausschließlich eigene Typen wie [`Solid`] und [`TriMesh`]
//! (siehe TECH_SPEC §6 und §11).

#![warn(missing_docs)]

use std::collections::{HashMap, HashSet};

use truck_meshalgo::prelude::*;
use truck_modeling::{builder, Point3, Rad, Vector3, Wire};

/// Ein B-Rep-Volumenkörper. Opaker Wrapper um `truck_modeling::Solid`.
pub struct Solid {
    inner: truck_modeling::Solid,
}

/// Fehler im Geometrie-Layer.
#[derive(Debug, thiserror::Error)]
pub enum GeomError {
    /// Profil hat zu wenige Punkte oder Null-Ausdehnung.
    #[error("Profil ist degeneriert")]
    DegenerateProfile,
    /// truck konnte aus dem Profil keine planare Fläche bauen.
    #[error("Fläche nicht erzeugbar: {0}")]
    FaceCreation(String),
    /// Dateisystem-Fehler beim Export.
    #[error("E/A-Fehler: {0}")]
    Io(#[from] std::io::Error),
}

/// Schreibt das Mesh als binäres STL (TECH_SPEC §8): 80-Byte-Header,
/// Dreieckszahl als u32, je Dreieck Normale + drei Vertices als f32
/// (little-endian) + 2 Attribut-Bytes.
pub fn export_stl(mesh: &TriMesh, path: &std::path::Path) -> Result<(), GeomError> {
    use std::io::Write;

    let triangle_count = (mesh.indices.len() / 3) as u32;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);

    let mut header = [0u8; 80];
    let tag = b"RustCAD binary STL";
    header[..tag.len()].copy_from_slice(tag);
    out.write_all(&header)?;
    out.write_all(&triangle_count.to_le_bytes())?;

    for tri in mesh.indices.chunks_exact(3) {
        let pos = |i: u32| {
            let p = mesh.positions[i as usize];
            [p[0] as f64, p[1] as f64, p[2] as f64]
        };
        let (a, b, c) = (pos(tri[0]), pos(tri[1]), pos(tri[2]));
        // STL erwartet die Flächennormale, nicht Vertex-Normalen
        let normal = triangle_normal(Point3::from(a), Point3::from(b), Point3::from(c));
        for component in normal {
            out.write_all(&component.to_le_bytes())?;
        }
        for vertex in [a, b, c] {
            for component in vertex {
                out.write_all(&(component as f32).to_le_bytes())?;
            }
        }
        out.write_all(&0u16.to_le_bytes())?;
    }
    out.flush()?;
    Ok(())
}

/// Ein geschlossenes Profil in 2D-Ebenen-Koordinaten.
#[derive(Debug, Clone)]
pub enum Profile2D {
    /// Geschlossener Polygonzug (ohne Wiederholung des Startpunkts).
    Polygon(Vec<[f64; 2]>),
    /// Vollkreis.
    Circle {
        /// Mittelpunkt.
        center: [f64; 2],
        /// Radius.
        radius: f64,
    },
}

/// Eine Ebene im Raum, aufgespannt durch `u` und `v` (Normale = u × v).
#[derive(Debug, Clone, Copy)]
pub struct Plane {
    /// Ursprung der Ebene.
    pub origin: [f64; 3],
    /// Erste Ebenen-Achse.
    pub u: [f64; 3],
    /// Zweite Ebenen-Achse.
    pub v: [f64; 3],
}

impl Plane {
    fn to_world(self, p: [f64; 2]) -> Point3 {
        Point3::new(
            self.origin[0] + self.u[0] * p[0] + self.v[0] * p[1],
            self.origin[1] + self.u[1] * p[0] + self.v[1] * p[1],
            self.origin[2] + self.u[2] * p[0] + self.v[2] * p[1],
        )
    }

    fn normal(self) -> Vector3 {
        Vector3::from(self.u)
            .cross(Vector3::from(self.v))
            .normalize()
    }

    fn dir_to_world(self, d: [f64; 2]) -> Vector3 {
        (Vector3::from(self.u) * d[0] + Vector3::from(self.v) * d[1]).normalize()
    }
}

/// Eine Achse in Ebenen-Koordinaten (für Revolve).
#[derive(Debug, Clone, Copy)]
pub struct Axis2D {
    /// Punkt auf der Achse.
    pub origin: [f64; 2],
    /// Richtung der Achse.
    pub dir: [f64; 2],
}

/// Extrudiert ein Profil senkrecht zur Ebene um `distance`
/// (negativ = entgegen der Normalen).
pub fn extrude(profile: &Profile2D, plane: &Plane, distance: f64) -> Result<Solid, GeomError> {
    if distance.abs() < 1e-9 {
        return Err(GeomError::DegenerateProfile);
    }
    let face = profile_face(profile, plane)?;
    let solid = builder::tsweep(&face, plane.normal() * distance);
    Ok(Solid { inner: solid })
}

/// Rotiert ein Profil um eine Achse in der Skizzierebene.
/// `angle` im Bogenmaß; |angle| ≥ 2π ergibt eine volle Rotation.
pub fn revolve(
    profile: &Profile2D,
    plane: &Plane,
    axis: &Axis2D,
    angle: f64,
) -> Result<Solid, GeomError> {
    if angle.abs() < 1e-6 {
        return Err(GeomError::DegenerateProfile);
    }
    let face = profile_face(profile, plane)?;
    let origin = plane.to_world(axis.origin);
    let direction = plane.dir_to_world(axis.dir);
    let solid = builder::rsweep(&face, origin, direction, Rad(angle));
    Ok(Solid { inner: solid })
}

/// Profil → truck `Wire` → planare Fläche.
fn profile_face(profile: &Profile2D, plane: &Plane) -> Result<truck_modeling::Face, GeomError> {
    let wire: Wire = match profile {
        Profile2D::Polygon(points) => {
            if points.len() < 3 {
                return Err(GeomError::DegenerateProfile);
            }
            let vertices: Vec<_> = points
                .iter()
                .map(|&p| builder::vertex(plane.to_world(p)))
                .collect();
            (0..vertices.len())
                .map(|i| builder::line(&vertices[i], &vertices[(i + 1) % vertices.len()]))
                .collect()
        }
        Profile2D::Circle { center, radius } => {
            if *radius <= 0.0 {
                return Err(GeomError::DegenerateProfile);
            }
            let [cx, cy] = *center;
            let right = builder::vertex(plane.to_world([cx + radius, cy]));
            let left = builder::vertex(plane.to_world([cx - radius, cy]));
            let top = plane.to_world([cx, cy + radius]);
            let bottom = plane.to_world([cx, cy - radius]);
            vec![
                builder::circle_arc(&right, &left, top),
                builder::circle_arc(&left, &right, bottom),
            ]
            .into_iter()
            .collect()
        }
    };
    builder::try_attach_plane(&[wire]).map_err(|e| GeomError::FaceCreation(e.to_string()))
}

/// Trianguliertes Mesh inklusive Kanten-Polylinien für die Darstellung
/// im Viewport.
#[derive(Debug, Clone, Default)]
pub struct TriMesh {
    /// Vertex-Positionen.
    pub positions: Vec<[f32; 3]>,
    /// Vertex-Normalen, parallel zu `positions`.
    pub normals: Vec<[f32; 3]>,
    /// Dreiecks-Indices in `positions`/`normals`; je drei bilden ein Dreieck.
    pub indices: Vec<u32>,
    /// B-Rep-Face-Index je Vertex (parallel zu `positions`) — Basis für
    /// das ID-Buffer-Picking im Viewport.
    pub face_ids: Vec<u32>,
    /// B-Rep-Kanten als Polylinien für die Kanten-Darstellung.
    pub edges: Vec<Vec<[f32; 3]>>,
}

impl TriMesh {
    /// Hängt ein anderes Mesh an — z. B. um mehrere Bodies in eine
    /// STL-Datei zu exportieren. Face-IDs bleiben Body-lokal.
    pub fn merge(&mut self, other: &TriMesh) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.normals.extend_from_slice(&other.normals);
        self.face_ids.extend_from_slice(&other.face_ids);
        self.indices.extend(other.indices.iter().map(|&i| base + i));
        self.edges.extend_from_slice(&other.edges);
    }

    /// Achsenparallele Bounding-Box als `(min, max)`.
    /// `None`, wenn das Mesh keine Vertices hat.
    pub fn bounding_box(&self) -> Option<([f32; 3], [f32; 3])> {
        let first = *self.positions.first()?;
        let (mut min, mut max) = (first, first);
        for p in &self.positions {
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        Some((min, max))
    }
}

/// Erzeugt einen achsenparallelen Würfel mit Kantenlänge `size`, dessen
/// Minimum-Ecke bei `origin` liegt.
///
/// Dient in Meilenstein 1 als End-to-End-Beweis der Kette
/// truck → Tessellierung → Rendering.
pub fn cube(origin: [f64; 3], size: f64) -> Solid {
    let vertex = builder::vertex(Point3::new(origin[0], origin[1], origin[2]));
    let edge = builder::tsweep(&vertex, Vector3::unit_x() * size);
    let face = builder::tsweep(&edge, Vector3::unit_y() * size);
    let solid = builder::tsweep(&face, Vector3::unit_z() * size);
    Solid { inner: solid }
}

/// Tesselliert einen Volumenkörper mit der gegebenen Toleranz
/// (maximale Abweichung des Meshes von der exakten Fläche).
///
/// Zu kleine Toleranzen werden auf die Kernel-Toleranz angehoben,
/// damit `truck` nicht panict.
pub fn tessellate(solid: &Solid, tolerance: f64) -> TriMesh {
    let tol = tolerance.max(TOLERANCE * 2.0);
    let meshed = solid.inner.triangulation(tol);

    let mut mesh = TriMesh::default();
    // Pro B-Rep-Face tessellieren, damit jeder Vertex seine Face-ID trägt
    for (face_id, face) in meshed.face_iter().enumerate() {
        let Some(mut polygon) = face.surface() else {
            continue;
        };
        if !face.orientation() {
            polygon.invert();
        }
        append_polygon(&mut mesh, &polygon, face_id as u32);
    }

    // Kanten-Polylinien; edge_iter liefert Kanten pro angrenzender Fläche,
    // daher Deduplizierung über die topologische ID.
    let mut seen = HashSet::new();
    for edge in meshed.edge_iter() {
        if seen.insert(edge.id()) {
            let polyline = edge.curve();
            mesh.edges.push(
                polyline
                    .0
                    .iter()
                    .map(|p| [p.x as f32, p.y as f32, p.z as f32])
                    .collect(),
            );
        }
    }

    mesh
}

/// Hängt ein trianguliertes Face an das Mesh an. truck indiziert Position
/// und Normale getrennt; fürs Rendern brauchen wir einen gemeinsamen
/// Index pro (Position, Normale)-Paar — pro Face dedupliziert.
fn append_polygon(mesh: &mut TriMesh, polygon: &PolygonMesh, face_id: u32) {
    let positions = polygon.positions();
    let normals = polygon.normals();
    let mut vertex_cache: HashMap<(usize, Option<usize>), u32> = HashMap::new();

    for tri in polygon.faces().triangle_iter() {
        let face_normal = triangle_normal(
            positions[tri[0].pos],
            positions[tri[1].pos],
            positions[tri[2].pos],
        );
        for v in tri {
            let index = *vertex_cache.entry((v.pos, v.nor)).or_insert_with(|| {
                let p = positions[v.pos];
                let n = v.nor.map_or(face_normal, |i| {
                    let n = normals[i];
                    [n.x as f32, n.y as f32, n.z as f32]
                });
                mesh.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                mesh.normals.push(n);
                mesh.face_ids.push(face_id);
                (mesh.positions.len() - 1) as u32
            });
            mesh.indices.push(index);
        }
    }
}

/// Normierte Flächennormale eines Dreiecks (Fallback, falls truck für
/// einen Vertex keine Normale liefert).
fn triangle_normal(a: Point3, b: Point3, c: Point3) -> [f32; 3] {
    let u = b - a;
    let v = c - a;
    let n = u.cross(v);
    let len = n.magnitude();
    if len <= f64::EPSILON {
        [0.0, 0.0, 1.0]
    } else {
        [(n.x / len) as f32, (n.y / len) as f32, (n.z / len) as f32]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signiertes Volumen über das Divergenz-Theorem
    /// (Summe der Tetraeder-Volumina gegen den Ursprung).
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
            .sum()
    }

    #[test]
    fn cube_tessellation_volume_and_bbox() {
        let solid = cube([-1.0, -1.0, 0.0], 2.0);
        let mesh = tessellate(&solid, 0.01);

        assert_eq!(mesh.positions.len(), mesh.normals.len());
        assert_eq!(mesh.indices.len() % 3, 0);
        assert!(!mesh.indices.is_empty());

        let volume = mesh_volume(&mesh).abs();
        assert!((volume - 8.0).abs() < 1e-6, "Volumen war {volume}");

        let (min, max) = mesh.bounding_box().expect("Mesh hat Vertices");
        assert_eq!(min, [-1.0, -1.0, 0.0]);
        assert_eq!(max, [1.0, 1.0, 2.0]);
    }

    #[test]
    fn cube_has_twelve_edges_and_six_faces() {
        let mesh = tessellate(&cube([0.0; 3], 1.0), 0.01);
        assert_eq!(mesh.edges.len(), 12);
        assert!(mesh.edges.iter().all(|e| e.len() >= 2));
        assert_eq!(mesh.face_ids.len(), mesh.positions.len());
        let max_face = mesh.face_ids.iter().max().copied().unwrap();
        assert_eq!(max_face, 5, "Würfel hat 6 Faces");
    }

    const XY: Plane = Plane {
        origin: [0.0; 3],
        u: [1.0, 0.0, 0.0],
        v: [0.0, 1.0, 0.0],
    };

    /// Golden-Test aus TECH_SPEC §10: 10×10-Quadrat um 5 → Volumen 500.
    #[test]
    fn extrude_square_golden_volume() {
        let profile = Profile2D::Polygon(vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]);
        let solid = extrude(&profile, &XY, 5.0).expect("Extrude");
        let mesh = tessellate(&solid, 0.01);
        let volume = mesh_volume(&mesh).abs();
        assert!((volume - 500.0).abs() < 1e-6, "Volumen war {volume}");
    }

    #[test]
    fn extrude_circle_volume_is_cylinder() {
        let profile = Profile2D::Circle {
            center: [2.0, 3.0],
            radius: 1.0,
        };
        let solid = extrude(&profile, &XY, 2.0).expect("Extrude");
        let mesh = tessellate(&solid, 0.001);
        let volume = mesh_volume(&mesh).abs();
        let expected = std::f64::consts::PI * 2.0;
        let relative = (volume - expected).abs() / expected;
        assert!(relative < 0.005, "Volumen {volume}, erwartet ≈ {expected}");
    }

    /// Pappus: V = 2π · x̄ · A. Quadrat [1,2]×[0,1] um die v-Achse
    /// (x = 0): A = 1, Schwerpunkt x̄ = 1.5 → V = 3π.
    #[test]
    fn revolve_square_full_turn_pappus() {
        let profile = Profile2D::Polygon(vec![[1.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0]]);
        let axis = Axis2D {
            origin: [0.0, 0.0],
            dir: [0.0, 1.0],
        };
        let solid = revolve(&profile, &XY, &axis, std::f64::consts::TAU).expect("Revolve");
        let mesh = tessellate(&solid, 0.001);
        let volume = mesh_volume(&mesh).abs();
        let expected = 3.0 * std::f64::consts::PI;
        let relative = (volume - expected).abs() / expected;
        assert!(relative < 0.005, "Volumen {volume}, erwartet ≈ {expected}");
    }

    #[test]
    fn stl_export_writes_valid_binary_layout() {
        let mesh = tessellate(&cube([0.0; 3], 2.0), 0.01);
        let path =
            std::env::temp_dir().join(format!("rustcad-stl-test-{}.stl", std::process::id()));
        export_stl(&mesh, &path).expect("STL-Export");

        let data = std::fs::read(&path).expect("STL lesen");
        let _ = std::fs::remove_file(&path);

        let triangle_count = mesh.indices.len() / 3;
        assert_eq!(data.len(), 84 + 50 * triangle_count);
        let count_in_file = u32::from_le_bytes(data[80..84].try_into().expect("u32"));
        assert_eq!(count_in_file as usize, triangle_count);
        // Erste Normale muss Einheitslänge haben
        let n: Vec<f32> = (0..3)
            .map(|i| f32::from_le_bytes(data[84 + 4 * i..88 + 4 * i].try_into().expect("f32")))
            .collect();
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-5, "Normale hatte Länge {len}");
    }

    #[test]
    fn merge_combines_meshes() {
        let mut a = tessellate(&cube([0.0; 3], 1.0), 0.01);
        let b = tessellate(&cube([2.0, 0.0, 0.0], 1.0), 0.01);
        let (a_pos, a_idx) = (a.positions.len(), a.indices.len());
        a.merge(&b);
        assert_eq!(a.positions.len(), a_pos + b.positions.len());
        assert_eq!(a.indices.len(), a_idx + b.indices.len());
        let volume = mesh_volume(&a);
        assert!((volume - 2.0).abs() < 1e-6, "Volumen war {volume}");
    }

    #[test]
    fn degenerate_profiles_are_rejected() {
        assert!(extrude(&Profile2D::Polygon(vec![[0.0, 0.0], [1.0, 0.0]]), &XY, 1.0).is_err());
        assert!(extrude(
            &Profile2D::Circle {
                center: [0.0, 0.0],
                radius: 0.0
            },
            &XY,
            1.0
        )
        .is_err());
        let square = Profile2D::Polygon(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        assert!(extrude(&square, &XY, 0.0).is_err());
    }
}
