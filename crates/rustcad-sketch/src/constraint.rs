//! Constraint-Definitionen und ihr Lowering auf skalare Gleichungen.
//!
//! Jeder Constraint wird auf eine oder mehrere skalare Gleichungen über
//! [`VarId`]s abgebildet. Die Gleichungen liefern Residuum und analytische
//! Jacobi-Zeile — der Solver kennt nur diese Darstellung (TECH_SPEC §5.2).

use serde::{Deserialize, Serialize};
use slotmap::new_key_type;

use crate::{EntityId, PointId, Sketch, SketchEntity, VarId};

new_key_type! {
    /// Stabile, generationsbasierte ID eines Constraints.
    pub struct ConstraintId;
}

/// Geometrischer Constraint (MVP-Satz aus TECH_SPEC §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Constraint {
    /// Zwei Punkte fallen zusammen.
    Coincident(PointId, PointId),
    /// Linie verläuft horizontal.
    Horizontal(EntityId),
    /// Linie verläuft vertikal.
    Vertical(EntityId),
    /// Zwei Linien sind parallel.
    Parallel(EntityId, EntityId),
    /// Zwei Linien stehen senkrecht aufeinander.
    Perpendicular(EntityId, EntityId),
    /// Abstand zweier Punkte.
    Distance(PointId, PointId, f64),
    /// Radius eines Kreises.
    Radius(EntityId, f64),
    /// Zwei Linien sind gleich lang.
    Equal(EntityId, EntityId),
}

/// Fehler beim Anlegen eines Constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConstraintError {
    /// Referenziertes Entity ist keine Linie.
    #[error("Entity ist keine Linie")]
    NotALine,
    /// Referenziertes Entity ist kein Kreis.
    #[error("Entity ist kein Kreis")]
    NotACircle,
    /// Punkt- oder Entity-Referenz existiert nicht (mehr).
    #[error("Referenz existiert nicht")]
    UnknownRef,
}

/// Eine skalare Gleichung `r(x) = 0` über dem Variablenvektor,
/// optional gewichtet (für weiche Drag-Constraints).
pub(crate) struct Equation {
    pub kind: EqKind,
    pub weight: f64,
}

impl Equation {
    pub fn new(kind: EqKind) -> Self {
        Self { kind, weight: 1.0 }
    }

    pub fn weighted(kind: EqKind, weight: f64) -> Self {
        Self { kind, weight }
    }

    pub fn residual(&self, var: &dyn Fn(VarId) -> f64) -> f64 {
        self.kind.residual(var) * self.weight
    }

    /// Ruft `add(var, ∂r/∂var)` für alle partiellen Ableitungen auf.
    /// Mehrfachnennungen derselben Variable akkumulieren beim Aufrufer.
    pub fn jacobian(&self, var: &dyn Fn(VarId) -> f64, add: &mut dyn FnMut(VarId, f64)) {
        let w = self.weight;
        self.kind.jacobian(var, &mut |id, d| add(id, d * w));
    }
}

/// Gleichungstypen. Linien sind durch ihre Endpunkt-Variablen
/// `[ax, ay, bx, by]` gegeben, Paare von Linien als `[a, b, c, e]`
/// (Richtungen `d1 = b − a`, `d2 = e − c`).
pub(crate) enum EqKind {
    /// `a − b = 0` (Koinzidenz je Achse, horizontal, vertikal)
    Diff(VarId, VarId),
    /// `v − value = 0` (Radius, Drag-Ziel)
    Value(VarId, f64),
    /// `‖p − q‖² − d² = 0`, Variablen `[px, py, qx, qy]`
    DistSq([VarId; 4], f64),
    /// `d1 × d2 = 0` (parallel), Variablen `[ax, ay, bx, by, cx, cy, ex, ey]`
    Cross([VarId; 8]),
    /// `d1 · d2 = 0` (senkrecht)
    Dot([VarId; 8]),
    /// `‖d1‖² − ‖d2‖² = 0` (gleich lang)
    EqualLenSq([VarId; 8]),
}

impl EqKind {
    fn residual(&self, var: &dyn Fn(VarId) -> f64) -> f64 {
        match *self {
            EqKind::Diff(a, b) => var(a) - var(b),
            EqKind::Value(v, value) => var(v) - value,
            EqKind::DistSq([px, py, qx, qy], d) => {
                (var(px) - var(qx)).powi(2) + (var(py) - var(qy)).powi(2) - d * d
            }
            EqKind::Cross(v) => {
                let (d1, d2) = dirs(&v, var);
                d1[0] * d2[1] - d1[1] * d2[0]
            }
            EqKind::Dot(v) => {
                let (d1, d2) = dirs(&v, var);
                d1[0] * d2[0] + d1[1] * d2[1]
            }
            EqKind::EqualLenSq(v) => {
                let (d1, d2) = dirs(&v, var);
                d1[0] * d1[0] + d1[1] * d1[1] - d2[0] * d2[0] - d2[1] * d2[1]
            }
        }
    }

    fn jacobian(&self, var: &dyn Fn(VarId) -> f64, add: &mut dyn FnMut(VarId, f64)) {
        match *self {
            EqKind::Diff(a, b) => {
                add(a, 1.0);
                add(b, -1.0);
            }
            EqKind::Value(v, _) => add(v, 1.0),
            EqKind::DistSq([px, py, qx, qy], _) => {
                let dx = var(px) - var(qx);
                let dy = var(py) - var(qy);
                add(px, 2.0 * dx);
                add(qx, -2.0 * dx);
                add(py, 2.0 * dy);
                add(qy, -2.0 * dy);
            }
            EqKind::Cross([ax, ay, bx, by, cx, cy, ex, ey]) => {
                let (d1, d2) = dirs(&[ax, ay, bx, by, cx, cy, ex, ey], var);
                add(ax, -d2[1]);
                add(bx, d2[1]);
                add(ay, d2[0]);
                add(by, -d2[0]);
                add(cx, d1[1]);
                add(ex, -d1[1]);
                add(cy, -d1[0]);
                add(ey, d1[0]);
            }
            EqKind::Dot([ax, ay, bx, by, cx, cy, ex, ey]) => {
                let (d1, d2) = dirs(&[ax, ay, bx, by, cx, cy, ex, ey], var);
                add(ax, -d2[0]);
                add(bx, d2[0]);
                add(ay, -d2[1]);
                add(by, d2[1]);
                add(cx, -d1[0]);
                add(ex, d1[0]);
                add(cy, -d1[1]);
                add(ey, d1[1]);
            }
            EqKind::EqualLenSq([ax, ay, bx, by, cx, cy, ex, ey]) => {
                let (d1, d2) = dirs(&[ax, ay, bx, by, cx, cy, ex, ey], var);
                add(ax, -2.0 * d1[0]);
                add(bx, 2.0 * d1[0]);
                add(ay, -2.0 * d1[1]);
                add(by, 2.0 * d1[1]);
                add(cx, 2.0 * d2[0]);
                add(ex, -2.0 * d2[0]);
                add(cy, 2.0 * d2[1]);
                add(ey, -2.0 * d2[1]);
            }
        }
    }
}

fn dirs(v: &[VarId; 8], var: &dyn Fn(VarId) -> f64) -> ([f64; 2], [f64; 2]) {
    (
        [var(v[2]) - var(v[0]), var(v[3]) - var(v[1])],
        [var(v[6]) - var(v[4]), var(v[7]) - var(v[5])],
    )
}

impl Sketch {
    /// Prüft, ob alle Referenzen eines Constraints (noch) gültig sind
    /// und die Entity-Arten passen.
    pub(crate) fn constraint_valid(&self, c: &Constraint) -> Result<(), ConstraintError> {
        let point = |id: PointId| {
            self.point(id)
                .map(|_| ())
                .ok_or(ConstraintError::UnknownRef)
        };
        let line = |id: EntityId| match self.entity(id) {
            Some(SketchEntity::Line { .. }) => Ok(()),
            Some(_) => Err(ConstraintError::NotALine),
            None => Err(ConstraintError::UnknownRef),
        };
        match *c {
            Constraint::Coincident(p, q) | Constraint::Distance(p, q, _) => point(p).and(point(q)),
            Constraint::Horizontal(l) | Constraint::Vertical(l) => line(l),
            Constraint::Parallel(l1, l2)
            | Constraint::Perpendicular(l1, l2)
            | Constraint::Equal(l1, l2) => line(l1).and(line(l2)),
            Constraint::Radius(c, _) => match self.entity(c) {
                Some(SketchEntity::Circle { .. }) => Ok(()),
                Some(_) => Err(ConstraintError::NotACircle),
                None => Err(ConstraintError::UnknownRef),
            },
        }
    }

    /// Senkt einen (gültigen) Constraint auf skalare Gleichungen ab.
    /// Constraints mit inzwischen ungültigen Referenzen liefern nichts.
    pub(crate) fn lower_constraint(&self, c: &Constraint, out: &mut Vec<Equation>) {
        let point_vars = |id: PointId| self.point(id).map(|p| (p.x, p.y));
        let line_vars = |id: EntityId| match self.entity(id) {
            Some(&SketchEntity::Line { p1, p2 }) => {
                let (ax, ay) = point_vars(p1)?;
                let (bx, by) = point_vars(p2)?;
                Some([ax, ay, bx, by])
            }
            _ => None,
        };
        let pair = |l1: EntityId, l2: EntityId| {
            let a = line_vars(l1)?;
            let b = line_vars(l2)?;
            Some([a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]])
        };

        match *c {
            Constraint::Coincident(p, q) => {
                if let (Some((px, py)), Some((qx, qy))) = (point_vars(p), point_vars(q)) {
                    out.push(Equation::new(EqKind::Diff(px, qx)));
                    out.push(Equation::new(EqKind::Diff(py, qy)));
                }
            }
            Constraint::Horizontal(l) => {
                if let Some([_, ay, _, by]) = line_vars(l) {
                    out.push(Equation::new(EqKind::Diff(ay, by)));
                }
            }
            Constraint::Vertical(l) => {
                if let Some([ax, _, bx, _]) = line_vars(l) {
                    out.push(Equation::new(EqKind::Diff(ax, bx)));
                }
            }
            Constraint::Parallel(l1, l2) => {
                if let Some(v) = pair(l1, l2) {
                    out.push(Equation::new(EqKind::Cross(v)));
                }
            }
            Constraint::Perpendicular(l1, l2) => {
                if let Some(v) = pair(l1, l2) {
                    out.push(Equation::new(EqKind::Dot(v)));
                }
            }
            Constraint::Distance(p, q, d) => {
                if let (Some((px, py)), Some((qx, qy))) = (point_vars(p), point_vars(q)) {
                    out.push(Equation::new(EqKind::DistSq([px, py, qx, qy], d)));
                }
            }
            Constraint::Radius(circle, r) => {
                if let Some(&SketchEntity::Circle { radius, .. }) = self.entity(circle) {
                    out.push(Equation::new(EqKind::Value(radius, r)));
                }
            }
            Constraint::Equal(l1, l2) => {
                if let Some(v) = pair(l1, l2) {
                    out.push(Equation::new(EqKind::EqualLenSq(v)));
                }
            }
        }
    }
}
