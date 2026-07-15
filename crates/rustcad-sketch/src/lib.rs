//! 2D-Skizze und Constraint-Solver von RustCAD.
//!
//! Dieses Crate ist headless (keine GUI-Abhängigkeiten). Alle Koordinaten
//! und Radien liegen in einem flachen Variablenvektor — der Constraint-
//! Solver (Meilenstein 3) arbeitet später direkt darauf (TECH_SPEC §5).
//!
//! Auch Snapping ([`Sketch::nearest_point`]) und Selektion
//! ([`Sketch::hit_test`]) leben hier, damit sie ohne GUI testbar sind.

#![warn(missing_docs)]

mod constraint;
mod dimension;
mod profiles;
mod solver;

pub use constraint::{Constraint, ConstraintError, ConstraintId};
pub use dimension::{Dimension, DimensionId, DimensionKind, DimensionTarget};
pub use profiles::Profile;
pub use solver::{SolveResult, SOLVE_TOLERANCE};

use serde::{Deserialize, Serialize};
use slotmap::{new_key_type, SlotMap};

new_key_type! {
    /// Stabile, generationsbasierte ID eines Skizzenpunkts.
    pub struct PointId;
    /// Stabile, generationsbasierte ID eines Skizzen-Entities.
    pub struct EntityId;
}

/// Index in den flachen Variablenvektor einer [`Sketch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VarId(usize);

/// Ein Skizzenpunkt: zwei Einträge im Variablenvektor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SketchPoint {
    /// Variable der X-Koordinate.
    pub x: VarId,
    /// Variable der Y-Koordinate.
    pub y: VarId,
}

/// Ein geometrisches Entity der Skizze.
///
/// Bögen und freie Punkte folgen mit Meilenstein 3/4 (TECH_SPEC §5.1).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SketchEntity {
    /// Strecke zwischen zwei Punkten.
    Line {
        /// Startpunkt.
        p1: PointId,
        /// Endpunkt.
        p2: PointId,
    },
    /// Kreis um einen Mittelpunkt.
    Circle {
        /// Mittelpunkt.
        center: PointId,
        /// Radius-Variable.
        radius: VarId,
    },
}

/// Eine 2D-Skizze in Ebenen-Koordinaten (u, v).
///
/// Gelöschte Entities geben ihre Variablen-Slots im MVP nicht frei;
/// der Vektor wächst monoton (für den Solver irrelevant, da nur
/// referenzierte Variablen eingehen).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sketch {
    vars: Vec<f64>,
    points: SlotMap<PointId, SketchPoint>,
    entities: SlotMap<EntityId, SketchEntity>,
    constraints: SlotMap<ConstraintId, Constraint>,
    /// Bemaßungen (Präsentationsschicht über den treibenden Constraints).
    /// `default` für Rückwärtskompatibilität mit `format_version` 1.
    #[serde(default)]
    dimensions: SlotMap<DimensionId, Dimension>,
}

impl Sketch {
    /// Leere Skizze.
    pub fn new() -> Self {
        Self::default()
    }

    fn alloc_var(&mut self, value: f64) -> VarId {
        self.vars.push(value);
        VarId(self.vars.len() - 1)
    }

    /// Wert einer Variablen.
    ///
    /// # Panics
    /// Bei ungültiger [`VarId`].
    pub fn var(&self, id: VarId) -> f64 {
        self.vars[id.0]
    }

    /// Setzt den Wert einer Variablen.
    ///
    /// # Panics
    /// Bei ungültiger [`VarId`].
    pub fn set_var(&mut self, id: VarId, value: f64) {
        self.vars[id.0] = value;
    }

    /// Legt einen neuen Punkt an.
    pub fn add_point(&mut self, pos: [f64; 2]) -> PointId {
        let x = self.alloc_var(pos[0]);
        let y = self.alloc_var(pos[1]);
        self.points.insert(SketchPoint { x, y })
    }

    /// Punkt-Variablen; `None` bei ungültiger ID.
    pub(crate) fn point(&self, id: PointId) -> Option<&SketchPoint> {
        self.points.get(id)
    }

    /// Position eines Punkts.
    ///
    /// # Panics
    /// Bei ungültiger [`PointId`].
    pub fn point_pos(&self, id: PointId) -> [f64; 2] {
        let p = self.points[id];
        [self.vars[p.x.0], self.vars[p.y.0]]
    }

    /// Verschiebt einen Punkt.
    ///
    /// # Panics
    /// Bei ungültiger [`PointId`].
    pub fn set_point_pos(&mut self, id: PointId, pos: [f64; 2]) {
        let p = self.points[id];
        self.vars[p.x.0] = pos[0];
        self.vars[p.y.0] = pos[1];
    }

    /// Fügt eine Strecke zwischen zwei existierenden Punkten hinzu.
    pub fn add_line(&mut self, p1: PointId, p2: PointId) -> EntityId {
        self.entities.insert(SketchEntity::Line { p1, p2 })
    }

    /// Fügt einen Kreis um einen existierenden Mittelpunkt hinzu.
    pub fn add_circle(&mut self, center: PointId, radius: f64) -> EntityId {
        let radius = self.alloc_var(radius);
        self.entities
            .insert(SketchEntity::Circle { center, radius })
    }

    /// Radius eines Kreises; `None` falls `id` kein Kreis (mehr) ist.
    pub fn circle_radius(&self, id: EntityId) -> Option<f64> {
        match self.entities.get(id)? {
            SketchEntity::Circle { radius, .. } => Some(self.vars[radius.0]),
            _ => None,
        }
    }

    /// Alle Punkte mit Position.
    pub fn points(&self) -> impl Iterator<Item = (PointId, [f64; 2])> + '_ {
        self.points
            .iter()
            .map(|(id, p)| (id, [self.vars[p.x.0], self.vars[p.y.0]]))
    }

    /// Alle Entities.
    pub fn entities(&self) -> impl Iterator<Item = (EntityId, &SketchEntity)> {
        self.entities.iter()
    }

    /// Ein einzelnes Entity; `None` bei ungültiger ID.
    pub fn entity(&self, id: EntityId) -> Option<&SketchEntity> {
        self.entities.get(id)
    }

    /// Anzahl der Entities.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Löscht ein Entity. Punkte, die danach von keinem Entity mehr
    /// referenziert werden, werden mit entfernt; Constraints mit
    /// ungültig gewordenen Referenzen ebenfalls.
    pub fn delete_entity(&mut self, id: EntityId) {
        let Some(entity) = self.entities.remove(id) else {
            return;
        };
        let candidates: &[PointId] = match entity {
            SketchEntity::Line { p1, p2 } => &[p1, p2],
            SketchEntity::Circle { center, .. } => &[center],
        };
        for &point in candidates {
            if !self.is_point_referenced(point) {
                self.points.remove(point);
            }
        }
        let invalid: Vec<ConstraintId> = self
            .constraints
            .iter()
            .filter(|(_, c)| self.constraint_valid(c).is_err())
            .map(|(id, _)| id)
            .collect();
        for c in invalid {
            self.constraints.remove(c);
        }
        // Bemaßungen, deren treibender Constraint gerade wegfiel, mitentfernen.
        self.prune_dangling_dimensions();
    }

    /// Fügt einen Constraint hinzu (validiert die Referenzen).
    /// Der Solver läuft nicht automatisch — [`Sketch::solve`] aufrufen.
    pub fn add_constraint(&mut self, c: Constraint) -> Result<ConstraintId, ConstraintError> {
        self.constraint_valid(&c)?;
        Ok(self.constraints.insert(c))
    }

    /// Entfernt einen Constraint. Eine daran hängende Bemaßung
    /// (die ihn als treibenden Constraint referenziert) fällt mit weg.
    pub fn delete_constraint(&mut self, id: ConstraintId) {
        self.constraints.remove(id);
        self.prune_dangling_dimensions();
    }

    /// Alle Constraints.
    pub fn constraints(&self) -> impl Iterator<Item = (ConstraintId, &Constraint)> {
        self.constraints.iter()
    }

    /// Ein einzelner Constraint; `None` bei ungültiger ID.
    pub fn constraint(&self, id: ConstraintId) -> Option<&Constraint> {
        self.constraints.get(id)
    }

    /// Anzahl der Constraints.
    pub fn constraint_count(&self) -> usize {
        self.constraints.len()
    }

    fn is_point_referenced(&self, point: PointId) -> bool {
        self.entities.values().any(|e| match *e {
            SketchEntity::Line { p1, p2 } => p1 == point || p2 == point,
            SketchEntity::Circle { center, .. } => center == point,
        })
    }

    /// Nächstgelegener Punkt innerhalb von `max_dist` (Snapping).
    pub fn nearest_point(&self, pos: [f64; 2], max_dist: f64) -> Option<(PointId, [f64; 2])> {
        self.points()
            .map(|(id, p)| (id, p, dist(p, pos)))
            .filter(|&(_, _, d)| d <= max_dist)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(id, p, _)| (id, p))
    }

    /// Entity unter dem Cursor (Selektion): das Entity mit dem kleinsten
    /// Abstand zu `pos` innerhalb der Toleranz `tol`.
    pub fn hit_test(&self, pos: [f64; 2], tol: f64) -> Option<EntityId> {
        self.entities
            .iter()
            .map(|(id, e)| (id, self.entity_distance(e, pos)))
            .filter(|&(_, d)| d <= tol)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    fn entity_distance(&self, entity: &SketchEntity, pos: [f64; 2]) -> f64 {
        match *entity {
            SketchEntity::Line { p1, p2 } => {
                dist_point_segment(pos, self.point_pos(p1), self.point_pos(p2))
            }
            SketchEntity::Circle { center, radius } => {
                (dist(self.point_pos(center), pos) - self.vars[radius.0]).abs()
            }
        }
    }
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Abstand eines Punkts zur Strecke `a`–`b`.
fn dist_point_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let len_sq = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len_sq <= f64::EPSILON {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / len_sq).clamp(0.0, 1.0)
    };
    dist(p, [a[0] + t * ab[0], a[1] + t * ab[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_sketch() -> (Sketch, PointId, PointId, EntityId) {
        let mut s = Sketch::new();
        let p1 = s.add_point([0.0, 0.0]);
        let p2 = s.add_point([10.0, 0.0]);
        let line = s.add_line(p1, p2);
        (s, p1, p2, line)
    }

    #[test]
    fn points_live_in_flat_var_vector() {
        let (s, p1, p2, _) = line_sketch();
        assert_eq!(s.vars.len(), 4);
        assert_eq!(s.point_pos(p1), [0.0, 0.0]);
        assert_eq!(s.point_pos(p2), [10.0, 0.0]);
    }

    #[test]
    fn snapping_picks_nearest_endpoint_within_radius() {
        let (s, p1, _, _) = line_sketch();
        let hit = s.nearest_point([0.3, 0.4], 1.0);
        assert_eq!(hit, Some((p1, [0.0, 0.0])));
        // außerhalb des Radius: kein Snap
        assert_eq!(s.nearest_point([3.0, 4.0], 1.0), None);
    }

    #[test]
    fn hit_test_finds_line_and_circle() {
        let (mut s, _, _, line) = line_sketch();
        let c = s.add_point([5.0, 5.0]);
        let circle = s.add_circle(c, 2.0);

        // nahe der Streckenmitte
        assert_eq!(s.hit_test([5.0, 0.2], 0.5), Some(line));
        // auf dem Kreisring (5, 5±2)
        assert_eq!(s.hit_test([5.0, 7.1], 0.5), Some(circle));
        // im Kreisinneren, weit weg vom Ring: nichts
        assert_eq!(s.hit_test([5.0, 5.0], 0.5), None);
    }

    #[test]
    fn hit_test_prefers_closest_entity() {
        let mut s = Sketch::new();
        let a = s.add_point([0.0, 0.0]);
        let b = s.add_point([10.0, 0.0]);
        let c = s.add_point([0.0, 1.0]);
        let d = s.add_point([10.0, 1.0]);
        let near = s.add_line(a, b);
        let _far = s.add_line(c, d);
        assert_eq!(s.hit_test([5.0, 0.3], 2.0), Some(near));
    }

    #[test]
    fn delete_removes_orphaned_points_but_keeps_shared() {
        let (mut s, p1, p2, line1) = line_sketch();
        let p3 = s.add_point([10.0, 5.0]);
        let _line2 = s.add_line(p2, p3);

        s.delete_entity(line1);
        // p1 war nur von line1 referenziert → weg; p2 wird von line2 geteilt
        assert!(s.points.get(p1).is_none());
        assert!(s.points.get(p2).is_some());
        assert_eq!(s.entity_count(), 1);
    }

    #[test]
    fn circle_radius_roundtrip() {
        let mut s = Sketch::new();
        let c = s.add_point([1.0, 2.0]);
        let circle = s.add_circle(c, 3.5);
        assert_eq!(s.circle_radius(circle), Some(3.5));
        s.delete_entity(circle);
        assert_eq!(s.circle_radius(circle), None);
        assert_eq!(s.entity_count(), 0);
    }
}
