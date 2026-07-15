//! Bemaßungen (Dimensions) als Präsentationsschicht über den Constraints.
//!
//! Eine Bemaßung ist kein eigenes Solver-Subsystem, sondern ein bestehender
//! *treibender* Constraint ([`Constraint::Distance`] bzw. [`Constraint::Radius`])
//! plus eine Anzeige-Annotation (Guardrail 5 der VISION):
//!
//! * der **Wert** lebt im Constraint (Solver-Domäne),
//! * die **Platzierung** lebt in der Annotation (Präsentationsschicht) —
//!   als Label-Offset *relativ* zur bemaßten Geometrie, ohne absolute Position.
//!   Bewegt der Solver die Geometrie, wandert die Bemaßung mit.
//!
//! Ein **Durchmesser** wird intern als Radius-Constraint (`value = r`)
//! gespeichert; nur die Anzeige zeigt `⌀ = 2r` ([`Sketch::dimension_value`]).

use serde::{Deserialize, Serialize};
use slotmap::new_key_type;

use crate::{Constraint, ConstraintError, ConstraintId, EntityId, PointId, SolveResult, Sketch};

new_key_type! {
    /// Stabile, generationsbasierte ID einer Bemaßung.
    pub struct DimensionId;
}

/// Art der Bemaßung — bestimmt, wie der Constraint-Wert angezeigt wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DimensionKind {
    /// Linearer Abstand zweier Punkte / Länge einer Linie
    /// (treibender Constraint: [`Constraint::Distance`]).
    Linear,
    /// Radius eines Kreises (treibender Constraint: [`Constraint::Radius`]).
    Radius,
    /// Durchmesser eines Kreises. Intern als Radius gespeichert
    /// (`value = r`); die Anzeige zeigt `⌀ = 2r`.
    Diameter,
}

/// Eine Bemaßung: Verweis auf den treibenden Constraint plus Platzierung.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Dimension {
    /// Der treibende Constraint, dessen Wert diese Bemaßung annotiert.
    pub constraint: ConstraintId,
    /// Welche Größe angezeigt wird (Linear / Radius / Durchmesser).
    pub kind: DimensionKind,
    /// Label-Offset *relativ* zur bemaßten Geometrie, in Skizzen-Koordinaten.
    /// Keine absolute Position — die Bemaßung folgt der Geometrie.
    pub offset: [f64; 2],
}

/// Was eine neue Bemaßung misst. Bestimmt den anzulegenden treibenden
/// Constraint und die [`DimensionKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimensionTarget {
    /// Abstand zweier Punkte (auch Linienlänge über ihre Endpunkte).
    Linear(PointId, PointId),
    /// Radius eines Kreises.
    Radius(EntityId),
    /// Durchmesser eines Kreises (intern als Radius gespeichert).
    Diameter(EntityId),
}

impl Sketch {
    /// Legt eine Bemaßung samt ihrem treibenden Constraint an.
    ///
    /// `value` ist der *angezeigte* Wert: bei [`DimensionTarget::Diameter`]
    /// der Durchmesser (intern als Radius `value / 2` gespeichert), sonst
    /// der Abstand bzw. Radius. `offset` ist der Label-Offset relativ zur
    /// Geometrie. Der Solver läuft **nicht** automatisch — [`Sketch::solve`]
    /// aufrufen (oder später [`Sketch::set_dimension_value`]).
    pub fn add_dimension(
        &mut self,
        target: DimensionTarget,
        value: f64,
        offset: [f64; 2],
    ) -> Result<DimensionId, ConstraintError> {
        let (constraint, kind) = match target {
            DimensionTarget::Linear(p, q) => {
                (Constraint::Distance(p, q, value), DimensionKind::Linear)
            }
            DimensionTarget::Radius(c) => (Constraint::Radius(c, value), DimensionKind::Radius),
            DimensionTarget::Diameter(c) => {
                (Constraint::Radius(c, value / 2.0), DimensionKind::Diameter)
            }
        };
        let constraint = self.add_constraint(constraint)?;
        Ok(self.dimensions.insert(Dimension {
            constraint,
            kind,
            offset,
        }))
    }

    /// Ändert den (angezeigten) Wert einer Bemaßung und löst die Skizze neu.
    ///
    /// Bei einer Durchmesser-Bemaßung wird intern der halbe Wert im
    /// Radius-Constraint abgelegt. Rückgabe ist das strukturierte
    /// [`SolveResult`]. Bei unbekannter `id` bleibt der Wert unverändert
    /// und die Skizze wird lediglich gelöst.
    pub fn set_dimension_value(&mut self, id: DimensionId, value: f64) -> SolveResult {
        if let Some(dim) = self.dimensions.get(id).copied() {
            let stored = match dim.kind {
                DimensionKind::Diameter => value / 2.0,
                DimensionKind::Linear | DimensionKind::Radius => value,
            };
            self.set_constraint_value(dim.constraint, stored);
        }
        self.solve()
    }

    /// Verschiebt nur das Label (Offset), ohne den Solver zu berühren.
    pub fn set_dimension_offset(&mut self, id: DimensionId, offset: [f64; 2]) {
        if let Some(dim) = self.dimensions.get_mut(id) {
            dim.offset = offset;
        }
    }

    /// Entfernt eine Bemaßung **samt ihrem treibenden Constraint**.
    pub fn remove_dimension(&mut self, id: DimensionId) {
        if let Some(dim) = self.dimensions.remove(id) {
            self.constraints.remove(dim.constraint);
        }
    }

    /// Der angezeigte Wert einer Bemaßung (`2r` bei Durchmesser), oder
    /// `None` bei unbekannter `id` bzw. inzwischen ungültigem Constraint.
    pub fn dimension_value(&self, id: DimensionId) -> Option<f64> {
        let dim = self.dimensions.get(id)?;
        let raw = self.constraint_value(dim.constraint)?;
        Some(match dim.kind {
            DimensionKind::Diameter => raw * 2.0,
            DimensionKind::Linear | DimensionKind::Radius => raw,
        })
    }

    /// Eine einzelne Bemaßung; `None` bei ungültiger ID.
    pub fn dimension(&self, id: DimensionId) -> Option<&Dimension> {
        self.dimensions.get(id)
    }

    /// Alle Bemaßungen.
    pub fn dimensions(&self) -> impl Iterator<Item = (DimensionId, &Dimension)> {
        self.dimensions.iter()
    }

    /// Anzahl der Bemaßungen.
    pub fn dimension_count(&self) -> usize {
        self.dimensions.len()
    }

    /// Liest den Wert eines treibenden Constraints (Distance/Radius).
    fn constraint_value(&self, id: ConstraintId) -> Option<f64> {
        match self.constraints.get(id)? {
            Constraint::Distance(_, _, v) | Constraint::Radius(_, v) => Some(*v),
            _ => None,
        }
    }

    /// Setzt den Wert eines treibenden Constraints (Distance/Radius).
    fn set_constraint_value(&mut self, id: ConstraintId, value: f64) {
        if let Some(Constraint::Distance(_, _, v) | Constraint::Radius(_, v)) =
            self.constraints.get_mut(id)
        {
            *v = value;
        }
    }

    /// Entfernt alle Bemaßungen, deren treibender Constraint nicht mehr
    /// existiert. Hält Bemaßungen und Constraints synchron, wenn Letztere
    /// direkt oder über Kaskaden (gelöschtes Entity) verschwinden.
    pub(crate) fn prune_dangling_dimensions(&mut self) {
        let dangling: Vec<DimensionId> = self
            .dimensions
            .iter()
            .filter(|(_, d)| !self.constraints.contains_key(d.constraint))
            .map(|(id, _)| id)
            .collect();
        for id in dangling {
            self.dimensions.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SolveResult, SOLVE_TOLERANCE};

    fn line(s: &mut Sketch) -> (PointId, PointId) {
        let p1 = s.add_point([0.0, 0.0]);
        let p2 = s.add_point([10.0, 0.0]);
        s.add_line(p1, p2);
        (p1, p2)
    }

    fn line_length(s: &Sketch, p1: PointId, p2: PointId) -> f64 {
        let a = s.point_pos(p1);
        let b = s.point_pos(p2);
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
    }

    /// Akzeptanzkriterium 1: Längenbemaßung 10 → auf 15 setzen → Solver
    /// konvergiert, Linienlänge == 15, Offset unverändert.
    #[test]
    fn set_linear_dimension_value_drives_the_solver() {
        let mut s = Sketch::new();
        let (p1, p2) = line(&mut s);
        let offset = [0.0, 2.0];
        let dim = s
            .add_dimension(DimensionTarget::Linear(p1, p2), 10.0, offset)
            .expect("linear dimension");
        assert_eq!(s.solve(), SolveResult::Solved { iterations: 0 });

        let result = s.set_dimension_value(dim, 15.0);
        assert!(matches!(result, SolveResult::Solved { .. }));
        assert!((line_length(&s, p1, p2) - 15.0).abs() < SOLVE_TOLERANCE.sqrt());
        assert_eq!(s.dimension_value(dim), Some(15.0));
        // Offset bleibt unberührt vom Solver.
        assert_eq!(s.dimension(dim).unwrap().offset, offset);
    }

    #[test]
    fn diameter_is_stored_as_radius_but_displayed_doubled() {
        let mut s = Sketch::new();
        let c = s.add_point([0.0, 0.0]);
        let circle = s.add_circle(c, 1.0);
        let dim = s
            .add_dimension(DimensionTarget::Diameter(circle), 8.0, [1.0, 1.0])
            .expect("diameter dimension");
        s.set_dimension_value(dim, 8.0);
        // Intern Radius = 4, Anzeige = 8 (Solver bis auf Toleranz).
        assert!((s.circle_radius(circle).unwrap() - 4.0).abs() < SOLVE_TOLERANCE.sqrt());
        assert!((s.dimension_value(dim).unwrap() - 8.0).abs() < SOLVE_TOLERANCE.sqrt());
    }

    #[test]
    fn removing_dimension_removes_driving_constraint() {
        let mut s = Sketch::new();
        let (p1, p2) = line(&mut s);
        let dim = s
            .add_dimension(DimensionTarget::Linear(p1, p2), 10.0, [0.0, 0.0])
            .expect("dimension");
        assert_eq!(s.constraint_count(), 1);
        s.remove_dimension(dim);
        assert_eq!(s.dimension_count(), 0);
        assert_eq!(s.constraint_count(), 0);
    }

    #[test]
    fn deleting_driving_constraint_removes_dimension() {
        let mut s = Sketch::new();
        let (p1, p2) = line(&mut s);
        let dim = s
            .add_dimension(DimensionTarget::Linear(p1, p2), 10.0, [0.0, 0.0])
            .expect("dimension");
        let cid = s.dimension(dim).unwrap().constraint;
        s.delete_constraint(cid);
        assert_eq!(s.dimension_count(), 0);
    }

    #[test]
    fn deleting_dimensioned_entity_cascades_to_dimension() {
        let mut s = Sketch::new();
        let c = s.add_point([0.0, 0.0]);
        let circle = s.add_circle(c, 2.0);
        let dim = s
            .add_dimension(DimensionTarget::Radius(circle), 2.0, [0.0, 0.0])
            .expect("dimension");
        s.delete_entity(circle);
        assert!(s.dimension(dim).is_none());
        assert_eq!(s.constraint_count(), 0);
    }

    #[test]
    fn set_dimension_offset_moves_only_the_label() {
        let mut s = Sketch::new();
        let (p1, p2) = line(&mut s);
        let dim = s
            .add_dimension(DimensionTarget::Linear(p1, p2), 10.0, [0.0, 1.0])
            .expect("dimension");
        s.set_dimension_offset(dim, [3.0, -2.0]);
        assert_eq!(s.dimension(dim).unwrap().offset, [3.0, -2.0]);
        // Wert unberührt.
        assert_eq!(s.dimension_value(dim), Some(10.0));
    }
}
