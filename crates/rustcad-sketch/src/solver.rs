//! Newton/Levenberg-Marquardt-Solver auf dem Variablenvektor.
//!
//! Strategie (TECH_SPEC §5.3): Residuenvektor `F(x) = 0`, analytische
//! Jacobi-Matrix, Schritt über die SVD von `J` mit LM-Dämpfung
//! `δ = −V · diag(σᵢ/(σᵢ² + λ)) · Uᵀ F`. Das ist für unterbestimmte
//! Systeme (der Normalfall) zugleich die Minimum-Norm-Lösung —
//! Startwerte sind die aktuellen Positionen, die Geometrie bewegt
//! sich also minimal ("least motion").

use std::collections::HashMap;

use nalgebra::{DMatrix, DVector};

use crate::constraint::{EqKind, Equation};
use crate::{PointId, Sketch, VarId};

/// Konvergenz: `‖F‖∞ < TOLERANCE`.
pub const SOLVE_TOLERANCE: f64 = 1e-9;
const MAX_ITERATIONS: usize = 50;
const MAX_DAMPING_RETRIES: usize = 12;
/// Drag-Gleichungen sind weich gewichtet, damit harte Constraints
/// bei Konflikten gewinnen.
const DRAG_WEIGHT: f64 = 0.25;

/// Ergebnis eines Solver-Laufs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SolveResult {
    /// Alle Residuen unter [`SOLVE_TOLERANCE`].
    Solved {
        /// Benötigte Newton/LM-Iterationen.
        iterations: usize,
    },
    /// Keine Konvergenz (widersprüchliche Constraints oder Iterations-
    /// limit). Die Variablen stehen auf der besten gefundenen
    /// Kleinste-Quadrate-Näherung.
    DidNotConverge {
        /// Verbleibendes Residuum `‖F‖∞`.
        residual: f64,
    },
}

impl Sketch {
    /// Löst alle Constraints der Skizze.
    pub fn solve(&mut self) -> SolveResult {
        self.solve_with_extra(Vec::new())
    }

    /// Löst mit der Drag-Position als temporärem, weich gewichtetem
    /// Constraint: der gezogene Punkt folgt der Maus, soweit die harten
    /// Constraints es zulassen.
    pub fn solve_drag(&mut self, point: PointId, target: [f64; 2]) -> SolveResult {
        let Some(p) = self.point(point).copied() else {
            return self.solve();
        };
        self.solve_with_extra(vec![
            Equation::weighted(EqKind::Value(p.x, target[0]), DRAG_WEIGHT),
            Equation::weighted(EqKind::Value(p.y, target[1]), DRAG_WEIGHT),
        ])
    }

    /// Verbleibende Freiheitsgrade: `freie Variablen − Rang(J)`
    /// an der aktuellen Konfiguration (TECH_SPEC §5.3).
    pub fn dof(&self) -> usize {
        let vars = self.active_vars();
        let equations = self.lowered_equations();
        if equations.is_empty() {
            return vars.len();
        }
        let index = var_index(&vars);
        let jacobian = self.build_jacobian(&equations, &vars, &index);
        let singular = jacobian.svd(false, false).singular_values;
        let max = singular.max();
        let rank = if max <= 0.0 {
            0
        } else {
            singular.iter().filter(|&&s| s > max * 1e-10).count()
        };
        vars.len().saturating_sub(rank)
    }

    /// Aktive Variablen in stabiler Reihenfolge: Punktkoordinaten,
    /// dann Kreisradien (gelöschte Slots tauchen nicht auf).
    fn active_vars(&self) -> Vec<VarId> {
        let mut vars = Vec::with_capacity(self.points.len() * 2 + self.entities.len());
        for (_, p) in self.points.iter() {
            vars.push(p.x);
            vars.push(p.y);
        }
        for (_, e) in self.entities.iter() {
            if let crate::SketchEntity::Circle { radius, .. } = e {
                vars.push(*radius);
            }
        }
        vars
    }

    fn lowered_equations(&self) -> Vec<Equation> {
        let mut out = Vec::new();
        for (_, c) in self.constraints.iter() {
            self.lower_constraint(c, &mut out);
        }
        out
    }

    fn build_jacobian(
        &self,
        equations: &[Equation],
        vars: &[VarId],
        index: &HashMap<VarId, usize>,
    ) -> DMatrix<f64> {
        let var = |id: VarId| self.var(id);
        let mut jacobian = DMatrix::zeros(equations.len(), vars.len());
        for (row, eq) in equations.iter().enumerate() {
            eq.jacobian(&var, &mut |id, d| {
                if let Some(&col) = index.get(&id) {
                    jacobian[(row, col)] += d;
                }
            });
        }
        jacobian
    }

    fn solve_with_extra(&mut self, extra: Vec<Equation>) -> SolveResult {
        let mut equations = self.lowered_equations();
        equations.extend(extra);
        if equations.is_empty() {
            return SolveResult::Solved { iterations: 0 };
        }

        let vars = self.active_vars();
        let index = var_index(&vars);
        let mut x: DVector<f64> =
            DVector::from_iterator(vars.len(), vars.iter().map(|&id| self.var(id)));

        let residuals = |sketch: &Sketch, x: &DVector<f64>| -> DVector<f64> {
            let var = |id: VarId| index.get(&id).map_or_else(|| sketch.var(id), |&i| x[i]);
            DVector::from_iterator(
                equations.len(),
                equations.iter().map(|eq| eq.residual(&var)),
            )
        };

        let mut lambda = 1e-4;
        let mut f = residuals(self, &x);

        for iteration in 0..MAX_ITERATIONS {
            if f.amax() < SOLVE_TOLERANCE {
                self.write_back(&vars, &x);
                return SolveResult::Solved {
                    iterations: iteration,
                };
            }

            // Jacobi an der aktuellen Stelle x (nicht an den Sketch-Vars)
            let var = |id: VarId| index.get(&id).map_or_else(|| self.var(id), |&i| x[i]);
            let mut jacobian = DMatrix::zeros(equations.len(), vars.len());
            for (row, eq) in equations.iter().enumerate() {
                eq.jacobian(&var, &mut |id, d| {
                    if let Some(&col) = index.get(&id) {
                        jacobian[(row, col)] += d;
                    }
                });
            }

            let svd = jacobian.svd(true, true);
            let (Some(u), Some(v_t)) = (svd.u.as_ref(), svd.v_t.as_ref()) else {
                break;
            };
            let ut_f = u.transpose() * &f;
            let current_norm = f.norm_squared();

            // LM-Dämpfung: λ erhöhen, bis der Schritt das Residuum senkt
            let mut accepted = false;
            for _ in 0..MAX_DAMPING_RETRIES {
                let mut scaled = ut_f.clone();
                for (i, s) in svd.singular_values.iter().enumerate() {
                    scaled[i] *= s / (s * s + lambda);
                }
                let x_new = &x - v_t.transpose() * scaled;
                let f_new = residuals(self, &x_new);
                if f_new.norm_squared() < current_norm {
                    x = x_new;
                    f = f_new;
                    lambda = (lambda / 3.0).max(1e-12);
                    accepted = true;
                    break;
                }
                lambda *= 10.0;
            }
            if !accepted {
                break;
            }
        }

        // Beste Näherung übernehmen (Drag-UX, Kleinste-Quadrate-Kompromiss)
        self.write_back(&vars, &x);
        SolveResult::DidNotConverge { residual: f.amax() }
    }

    fn write_back(&mut self, vars: &[VarId], x: &DVector<f64>) {
        for (i, &id) in vars.iter().enumerate() {
            self.set_var(id, x[i]);
        }
    }
}

fn var_index(vars: &[VarId]) -> HashMap<VarId, usize> {
    vars.iter().enumerate().map(|(i, &v)| (v, i)).collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use crate::{Constraint, Sketch, SolveResult, VarId};

    /// Linie 1: (0,0)–(10,1); Linie 2: (0,5)–(8,9); Kreis um (3,−2), r=2.
    fn test_sketch() -> (Sketch, [crate::EntityId; 3], [crate::PointId; 4]) {
        let mut s = Sketch::new();
        let a = s.add_point([0.0, 0.0]);
        let b = s.add_point([10.0, 1.0]);
        let c = s.add_point([0.0, 5.0]);
        let d = s.add_point([8.0, 9.0]);
        let center = s.add_point([3.0, -2.0]);
        let l1 = s.add_line(a, b);
        let l2 = s.add_line(c, d);
        let circle = s.add_circle(center, 2.0);
        (s, [l1, l2, circle], [a, b, c, d])
    }

    fn max_residual(sketch: &Sketch) -> f64 {
        let var = |id: VarId| sketch.var(id);
        sketch
            .lowered_equations()
            .iter()
            .map(|eq| eq.residual(&var).abs())
            .fold(0.0, f64::max)
    }

    /// Spec §10: analytische Jacobi gegen zentrale Differenz prüfen —
    /// für jeden der acht Constraint-Typen.
    #[test]
    fn jacobian_matches_numerical_difference() {
        let (s, [l1, l2, circle], [a, b, c, _]) = test_sketch();
        let constraints = [
            Constraint::Coincident(a, c),
            Constraint::Horizontal(l1),
            Constraint::Vertical(l1),
            Constraint::Parallel(l1, l2),
            Constraint::Perpendicular(l1, l2),
            Constraint::Distance(a, b, 7.0),
            Constraint::Radius(circle, 3.0),
            Constraint::Equal(l1, l2),
        ];

        for constraint in constraints {
            let mut equations = Vec::new();
            s.lower_constraint(&constraint, &mut equations);
            assert!(!equations.is_empty(), "{constraint:?} ohne Gleichungen");

            for eq in &equations {
                let mut analytic: std::collections::HashMap<VarId, f64> =
                    std::collections::HashMap::new();
                eq.jacobian(&|id| s.var(id), &mut |id, d| {
                    *analytic.entry(id).or_insert(0.0) += d;
                });

                let h = 1e-6;
                for &id in s.active_vars().iter() {
                    let eval = |value: f64| {
                        eq.residual(&|v: VarId| if v == id { value } else { s.var(v) })
                    };
                    let base = s.var(id);
                    let numeric = (eval(base + h) - eval(base - h)) / (2.0 * h);
                    let exact = analytic.get(&id).copied().unwrap_or(0.0);
                    assert!(
                        (numeric - exact).abs() < 1e-5,
                        "{constraint:?}: ∂/∂{id:?} analytisch {exact}, numerisch {numeric}"
                    );
                }
            }
        }
    }

    #[test]
    fn horizontal_solves_with_least_motion() {
        let (mut s, [l1, ..], [a, b, ..]) = test_sketch();
        s.add_constraint(Constraint::Horizontal(l1)).unwrap();
        assert!(matches!(s.solve(), SolveResult::Solved { .. }));

        let (pa, pb) = (s.point_pos(a), s.point_pos(b));
        assert!((pa[1] - pb[1]).abs() < 1e-9);
        // Least motion: beide Enden bewegen sich je ~0.5 aufeinander zu,
        // statt dass ein Ende um 1.0 springt; x bleibt unangetastet.
        assert!((pa[1] - 0.5).abs() < 1e-6, "p1.y = {}", pa[1]);
        assert!((pb[1] - 0.5).abs() < 1e-6, "p2.y = {}", pb[1]);
        assert!((pa[0] - 0.0).abs() < 1e-9 && (pb[0] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn coincident_pulls_points_together() {
        let (mut s, _, [a, _, c, _]) = test_sketch();
        s.add_constraint(Constraint::Coincident(a, c)).unwrap();
        assert!(matches!(s.solve(), SolveResult::Solved { .. }));
        let (pa, pc) = (s.point_pos(a), s.point_pos(c));
        assert!((pa[0] - pc[0]).abs() < 1e-9 && (pa[1] - pc[1]).abs() < 1e-9);
    }

    #[test]
    fn rectangle_constraints_solve_and_reduce_dof() {
        let mut s = Sketch::new();
        let bl = s.add_point([0.1, -0.2]);
        let br = s.add_point([9.7, 0.3]);
        let tr = s.add_point([10.2, 5.4]);
        let tl = s.add_point([-0.3, 4.8]);
        let bottom = s.add_line(bl, br);
        let right = s.add_line(br, tr);
        let top = s.add_line(tr, tl);
        let left = s.add_line(tl, bl);

        // 8 freie Variablen, keine Constraints
        assert_eq!(s.dof(), 8);

        s.add_constraint(Constraint::Horizontal(bottom)).unwrap();
        s.add_constraint(Constraint::Horizontal(top)).unwrap();
        s.add_constraint(Constraint::Vertical(left)).unwrap();
        s.add_constraint(Constraint::Vertical(right)).unwrap();
        s.add_constraint(Constraint::Distance(bl, br, 10.0))
            .unwrap();
        s.add_constraint(Constraint::Distance(bl, tl, 5.0)).unwrap();

        assert!(matches!(s.solve(), SolveResult::Solved { .. }));
        assert!(max_residual(&s) < 1e-9);
        // Übrig: Translation in x und y
        assert_eq!(s.dof(), 2);
    }

    #[test]
    fn contradictory_constraints_do_not_converge() {
        let (mut s, [l1, ..], [a, b, ..]) = test_sketch();
        s.add_constraint(Constraint::Horizontal(l1)).unwrap();
        s.add_constraint(Constraint::Vertical(l1)).unwrap();
        // H + V erzwingen Länge 0 — Abstand 5 ist damit unerfüllbar
        s.add_constraint(Constraint::Distance(a, b, 5.0)).unwrap();
        assert!(matches!(s.solve(), SolveResult::DidNotConverge { .. }));
    }

    #[test]
    fn drag_respects_hard_constraints() {
        let (mut s, [l1, ..], [a, b, ..]) = test_sketch();
        s.add_constraint(Constraint::Horizontal(l1)).unwrap();
        s.solve();

        s.solve_drag(a, [-3.0, 4.0]);
        let (pa, pb) = (s.point_pos(a), s.point_pos(b));
        // Linie bleibt horizontal, der gezogene Punkt folgt der Maus
        assert!((pa[1] - pb[1]).abs() < 1e-6);
        assert!((pa[0] + 3.0).abs() < 1e-6 && (pa[1] - 4.0).abs() < 1e-6);
    }

    #[test]
    fn radius_constraint_drives_circle() {
        let (mut s, [_, _, circle], _) = test_sketch();
        s.add_constraint(Constraint::Radius(circle, 3.5)).unwrap();
        assert!(matches!(s.solve(), SolveResult::Solved { .. }));
        assert!((s.circle_radius(circle).unwrap() - 3.5).abs() < 1e-9);
    }

    #[test]
    fn deleting_entity_removes_dependent_constraints() {
        let (mut s, [l1, l2, _], _) = test_sketch();
        s.add_constraint(Constraint::Parallel(l1, l2)).unwrap();
        s.add_constraint(Constraint::Horizontal(l2)).unwrap();
        s.delete_entity(l1);
        // Parallel referenzierte l1 → weg; Horizontal(l2) bleibt
        assert_eq!(s.constraint_count(), 1);
    }

    proptest! {
        /// Spec M3: zufällige lösbare Systeme konvergieren, und gelöste
        /// Systeme erfüllen alle Residuen < 1e-9. Konstruktion: exakt
        /// lösbares Rechteck, zufällig gestört.
        #[test]
        fn perturbed_rectangles_converge(
            width in 1.0..20.0f64,
            height in 1.0..20.0f64,
            origin_x in -10.0..10.0f64,
            origin_y in -10.0..10.0f64,
            noise in proptest::collection::vec(-0.3..0.3f64, 8),
        ) {
            let mut s = Sketch::new();
            let corners = [
                [origin_x, origin_y],
                [origin_x + width, origin_y],
                [origin_x + width, origin_y + height],
                [origin_x, origin_y + height],
            ];
            let pts: Vec<_> = corners
                .iter()
                .enumerate()
                .map(|(i, c)| s.add_point([c[0] + noise[2 * i], c[1] + noise[2 * i + 1]]))
                .collect();
            let bottom = s.add_line(pts[0], pts[1]);
            let right = s.add_line(pts[1], pts[2]);
            let top = s.add_line(pts[2], pts[3]);
            let left = s.add_line(pts[3], pts[0]);

            s.add_constraint(Constraint::Horizontal(bottom)).unwrap();
            s.add_constraint(Constraint::Vertical(right)).unwrap();
            s.add_constraint(Constraint::Parallel(top, bottom)).unwrap();
            s.add_constraint(Constraint::Perpendicular(left, bottom)).unwrap();
            s.add_constraint(Constraint::Distance(pts[0], pts[1], width)).unwrap();
            s.add_constraint(Constraint::Equal(left, right)).unwrap();

            prop_assert!(matches!(s.solve(), SolveResult::Solved { .. }), "Solver konvergierte nicht");
            prop_assert!(max_residual(&s) < 1e-9);
        }

        /// Kette horizontaler/vertikaler Segmente mit Koinzidenz am Start.
        #[test]
        fn perturbed_hv_chains_converge(
            steps in proptest::collection::vec((1.0..5.0f64, prop::bool::ANY), 2..6),
            noise_seed in 0u64..1000,
        ) {
            let mut s = Sketch::new();
            let mut pos = [0.0, 0.0];
            let mut prev = s.add_point(pos);
            let mut lines = Vec::new();
            let mut rng = noise_seed;
            let mut noise = move || {
                // einfacher LCG reicht als deterministisches Rauschen
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((rng >> 33) as f64 / 2f64.powi(31) - 0.5) * 0.4
            };
            for &(len, horizontal) in &steps {
                if horizontal {
                    pos[0] += len;
                } else {
                    pos[1] += len;
                }
                let next = s.add_point([pos[0] + noise(), pos[1] + noise()]);
                lines.push((s.add_line(prev, next), horizontal));
                prev = next;
            }
            for &(line, horizontal) in &lines {
                let c = if horizontal {
                    Constraint::Horizontal(line)
                } else {
                    Constraint::Vertical(line)
                };
                s.add_constraint(c).unwrap();
            }

            prop_assert!(matches!(s.solve(), SolveResult::Solved { .. }), "Solver konvergierte nicht");
            prop_assert!(max_residual(&s) < 1e-9);
        }
    }
}
