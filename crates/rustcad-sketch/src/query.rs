//! Headless Abfragen über Constraints (Guardrail 2: reine Queries, keine
//! Mutation). Sie machen Constraints für die Präsentationsschicht sichtbar,
//! ohne dort Sketch-Interna offenzulegen: Welche Constraints hängen an einem
//! Element? Welcher Art ist ein Constraint, was referenziert er, treibt er
//! eine Bemaßung? Die Glyphen-Platzierung im Overlay ist reine Präsentation
//! und lebt in der App — hier fällt keine GUI-Entscheidung.

use crate::{Constraint, ConstraintId, DimensionId, EntityId, PointId, Sketch};

/// Art eines Constraints ohne seine Referenzdaten — für die Glyphen-Auswahl
/// und die Anzeige im Eigenschaften-Panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    /// Zwei Punkte fallen zusammen.
    Coincident,
    /// Linie verläuft horizontal.
    Horizontal,
    /// Linie verläuft vertikal.
    Vertical,
    /// Zwei Linien sind parallel.
    Parallel,
    /// Zwei Linien stehen senkrecht aufeinander.
    Perpendicular,
    /// Abstand zweier Punkte (treibend bei Längen-Bemaßungen).
    Distance,
    /// Radius eines Kreises (treibend bei Radius-/Durchmesser-Bemaßungen).
    Radius,
    /// Zwei Linien sind gleich lang.
    Equal,
}

/// Ein von einem Constraint referenziertes Element: ein Punkt oder ein
/// Entity. Basis für Cross-Highlighting (Constraint ↔ Geometrie).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintRef {
    /// Referenz auf einen Skizzenpunkt.
    Point(PointId),
    /// Referenz auf ein Entity (Linie/Kreis).
    Entity(EntityId),
}

/// Aufgeschlüsselte Beschreibung eines Constraints: Art, referenzierte
/// Elemente und — falls er eine Bemaßung treibt — deren ID.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstraintInfo {
    /// Art des Constraints (ohne Referenzdaten).
    pub kind: ConstraintKind,
    /// Referenzierte Punkte/Entities (Reihenfolge wie im Constraint).
    pub refs: Vec<ConstraintRef>,
    /// Gesetzt, wenn dieser Constraint eine Bemaßung *treibt* — dann zeigt
    /// bereits die Bemaßungs-Annotation die Beziehung an (kein Glyph nötig).
    pub dimension: Option<DimensionId>,
}

/// Referenzierte Elemente eines Constraints in Constraint-Reihenfolge.
fn refs_of(c: &Constraint) -> Vec<ConstraintRef> {
    use ConstraintRef::{Entity, Point};
    match *c {
        Constraint::Coincident(p, q) | Constraint::Distance(p, q, _) => vec![Point(p), Point(q)],
        Constraint::Horizontal(l) | Constraint::Vertical(l) | Constraint::Radius(l, _) => {
            vec![Entity(l)]
        }
        Constraint::Parallel(a, b) | Constraint::Perpendicular(a, b) | Constraint::Equal(a, b) => {
            vec![Entity(a), Entity(b)]
        }
    }
}

/// Art eines Constraints.
fn kind_of(c: &Constraint) -> ConstraintKind {
    match c {
        Constraint::Coincident(..) => ConstraintKind::Coincident,
        Constraint::Horizontal(_) => ConstraintKind::Horizontal,
        Constraint::Vertical(_) => ConstraintKind::Vertical,
        Constraint::Parallel(..) => ConstraintKind::Parallel,
        Constraint::Perpendicular(..) => ConstraintKind::Perpendicular,
        Constraint::Distance(..) => ConstraintKind::Distance,
        Constraint::Radius(..) => ConstraintKind::Radius,
        Constraint::Equal(..) => ConstraintKind::Equal,
    }
}

impl Sketch {
    /// Alle Constraints, die `element` (Punkt oder Entity) referenzieren.
    /// Reihenfolge folgt der Constraint-Iteration; leer bei unbekanntem
    /// Element.
    pub fn constraints_on(&self, element: ConstraintRef) -> Vec<ConstraintId> {
        self.constraints()
            .filter(|(_, c)| refs_of(c).contains(&element))
            .map(|(id, _)| id)
            .collect()
    }

    /// Aufgeschlüsselte Beschreibung eines Constraints (Art, Referenzen,
    /// treibende Bemaßung), oder `None` bei unbekannter ID.
    pub fn constraint_info(&self, id: ConstraintId) -> Option<ConstraintInfo> {
        let c = self.constraint(id)?;
        Some(ConstraintInfo {
            kind: kind_of(c),
            refs: refs_of(c),
            dimension: self.driving_dimension(id),
        })
    }

    /// Die Bemaßung, die von `constraint` getrieben wird, falls es eine gibt.
    fn driving_dimension(&self, constraint: ConstraintId) -> Option<DimensionId> {
        self.dimensions()
            .find(|(_, d)| d.constraint == constraint)
            .map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Constraint, DimensionTarget};

    fn rectangle() -> (Sketch, [EntityId; 4], [PointId; 4]) {
        let mut s = Sketch::new();
        let bl = s.add_point([0.0, 0.0]);
        let br = s.add_point([10.0, 0.0]);
        let tr = s.add_point([10.0, 6.0]);
        let tl = s.add_point([0.0, 6.0]);
        let bottom = s.add_line(bl, br);
        let right = s.add_line(br, tr);
        let top = s.add_line(tr, tl);
        let left = s.add_line(tl, bl);
        (s, [bottom, right, top, left], [bl, br, tr, tl])
    }

    #[test]
    fn constraint_info_reports_kind_and_refs() {
        let (mut s, [bottom, right, ..], [bl, br, ..]) = rectangle();
        let hor = s.add_constraint(Constraint::Horizontal(bottom)).unwrap();
        let perp = s.add_constraint(Constraint::Perpendicular(bottom, right)).unwrap();
        let coin = s.add_constraint(Constraint::Coincident(bl, br)).unwrap();

        let h = s.constraint_info(hor).unwrap();
        assert_eq!(h.kind, ConstraintKind::Horizontal);
        assert_eq!(h.refs, vec![ConstraintRef::Entity(bottom)]);
        assert_eq!(h.dimension, None);

        let p = s.constraint_info(perp).unwrap();
        assert_eq!(p.kind, ConstraintKind::Perpendicular);
        assert_eq!(
            p.refs,
            vec![ConstraintRef::Entity(bottom), ConstraintRef::Entity(right)]
        );

        let c = s.constraint_info(coin).unwrap();
        assert_eq!(c.kind, ConstraintKind::Coincident);
        assert_eq!(
            c.refs,
            vec![ConstraintRef::Point(bl), ConstraintRef::Point(br)]
        );
    }

    #[test]
    fn constraints_on_finds_by_entity_and_point() {
        let (mut s, [bottom, right, top, _left], [bl, br, ..]) = rectangle();
        let hor = s.add_constraint(Constraint::Horizontal(bottom)).unwrap();
        let perp = s
            .add_constraint(Constraint::Perpendicular(bottom, right))
            .unwrap();
        let par = s.add_constraint(Constraint::Parallel(top, bottom)).unwrap();
        let coin = s.add_constraint(Constraint::Coincident(bl, br)).unwrap();

        // `bottom` steckt in Horizontal, Perpendicular und Parallel.
        let mut on_bottom = s.constraints_on(ConstraintRef::Entity(bottom));
        on_bottom.sort();
        let mut expected = vec![hor, perp, par];
        expected.sort();
        assert_eq!(on_bottom, expected);

        // `right` nur im Perpendicular.
        assert_eq!(s.constraints_on(ConstraintRef::Entity(right)), vec![perp]);
        // `bl` nur im Coincident.
        assert_eq!(s.constraints_on(ConstraintRef::Point(bl)), vec![coin]);
    }

    #[test]
    fn constraint_info_flags_driving_dimension() {
        let (mut s, _, [bl, br, ..]) = rectangle();
        let dim = s
            .add_dimension(DimensionTarget::Linear(bl, br), 10.0, [0.0, -2.0])
            .expect("dimension");
        let driver = s.dimension(dim).unwrap().constraint;

        let info = s.constraint_info(driver).unwrap();
        assert_eq!(info.kind, ConstraintKind::Distance);
        assert_eq!(info.dimension, Some(dim));
    }

    /// Akzeptanz 2 (headless): einen Perpendicular-Constraint löschen gibt
    /// genau einen Freiheitsgrad frei.
    #[test]
    fn deleting_perpendicular_frees_one_dof() {
        let (mut s, [bottom, right, ..], _) = rectangle();
        let perp = s
            .add_constraint(Constraint::Perpendicular(bottom, right))
            .unwrap();
        let dof_with = s.dof();
        s.delete_constraint(perp);
        assert_eq!(s.dof(), dof_with + 1);
        // Glyph-Quelle verschwindet mit: keine Info mehr für die ID.
        assert!(s.constraint_info(perp).is_none());
    }

    /// Akzeptanz 3 (headless): den treibenden Constraint einer Bemaßung
    /// löschen entfernt die Bemaßung mit (keine baumelnde Annotation).
    #[test]
    fn deleting_driving_constraint_removes_dimension() {
        let (mut s, _, [bl, br, ..]) = rectangle();
        let dim = s
            .add_dimension(DimensionTarget::Linear(bl, br), 10.0, [0.0, -2.0])
            .expect("dimension");
        let driver = s.dimension(dim).unwrap().constraint;
        assert_eq!(s.dimension_count(), 1);

        s.delete_constraint(driver);
        assert_eq!(s.dimension_count(), 0);
        assert!(s.dimension(dim).is_none());
    }
}
