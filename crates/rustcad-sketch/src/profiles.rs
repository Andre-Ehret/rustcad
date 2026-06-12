//! Profil-Erkennung: geschlossene Schleifen für Extrude/Revolve
//! (TECH_SPEC §5.4).
//!
//! MVP: nur einfache, nicht-verschachtelte Schleifen — jeder Kreis ist
//! ein Profil; eine Linien-Komponente ist genau dann ein Profil, wenn
//! sie ein einzelner einfacher Zyklus ist (jeder Knoten Grad 2,
//! mindestens drei Kanten). Über Koinzidenz-Constraints verbundene
//! Punkte zählen als ein Knoten.

use std::collections::HashMap;

use crate::{Constraint, PointId, Sketch, SketchEntity};

/// Eine geschlossene Region der Skizze in Ebenen-Koordinaten.
#[derive(Debug, Clone, PartialEq)]
pub enum Profile {
    /// Geschlossener Polygonzug, gegen den Uhrzeigersinn orientiert,
    /// ohne Wiederholung des Startpunkts.
    Polygon(Vec<[f64; 2]>),
    /// Vollkreis.
    Circle {
        /// Mittelpunkt.
        center: [f64; 2],
        /// Radius.
        radius: f64,
    },
}

impl Sketch {
    /// Findet alle geschlossenen Profile der Skizze.
    pub fn find_profiles(&self) -> Vec<Profile> {
        let mut profiles = Vec::new();

        // Kreise sind trivial geschlossen
        for (id, entity) in self.entities() {
            if let SketchEntity::Circle { center, .. } = *entity {
                if let Some(radius) = self.circle_radius(id) {
                    if radius > 0.0 {
                        profiles.push(Profile::Circle {
                            center: self.point_pos(center),
                            radius,
                        });
                    }
                }
            }
        }

        // Koinzidente Punkte zu Knoten verschmelzen (Union-Find)
        let mut union_find = UnionFind::default();
        for (_, c) in self.constraints() {
            if let Constraint::Coincident(p, q) = *c {
                union_find.union(p, q);
            }
        }

        // Linien-Graph: Knoten → angrenzende Kanten (als Knotenpaare)
        let mut adjacency: HashMap<PointId, Vec<(PointId, PointId)>> = HashMap::new();
        for (_, entity) in self.entities() {
            if let SketchEntity::Line { p1, p2 } = *entity {
                let (a, b) = (union_find.find(p1), union_find.find(p2));
                if a == b {
                    continue; // degenerierte Linie
                }
                adjacency.entry(a).or_default().push((a, b));
                adjacency.entry(b).or_default().push((a, b));
            }
        }

        // Komponenten ablaufen: einfacher Zyklus ⇔ überall Grad 2.
        // Startknoten in Entity-Reihenfolge, damit die Profil-Indizes
        // über Rebuilds stabil bleiben (ProfileSelection!).
        let mut visited: HashMap<PointId, bool> = HashMap::new();
        let mut nodes: Vec<PointId> = Vec::new();
        for (_, entity) in self.entities() {
            if let SketchEntity::Line { p1, p2 } = *entity {
                nodes.push(union_find.find(p1));
                nodes.push(union_find.find(p2));
            }
        }
        for start in nodes {
            if !adjacency.contains_key(&start) {
                continue;
            }
            if visited.contains_key(&start) {
                continue;
            }
            if let Some(loop_nodes) = trace_cycle(start, &adjacency, &mut visited) {
                if loop_nodes.len() >= 3 {
                    let mut points: Vec<[f64; 2]> = loop_nodes
                        .iter()
                        .map(|&node| self.point_pos(node))
                        .collect();
                    if signed_area(&points) < 0.0 {
                        points.reverse();
                    }
                    profiles.push(Profile::Polygon(points));
                }
            }
        }

        profiles
    }
}

/// Läuft die Komponente von `start` ab. Liefert die Knotenfolge des
/// Zyklus, wenn die Komponente ein einzelner einfacher Zyklus ist
/// (jeder Knoten Grad 2), sonst `None`. Markiert alle besuchten Knoten.
fn trace_cycle(
    start: PointId,
    adjacency: &HashMap<PointId, Vec<(PointId, PointId)>>,
    visited: &mut HashMap<PointId, bool>,
) -> Option<Vec<PointId>> {
    // Erst die ganze Komponente einsammeln und Grade prüfen
    let mut stack = vec![start];
    let mut component = Vec::new();
    let mut all_degree_two = true;
    while let Some(node) = stack.pop() {
        if visited.insert(node, true).is_some() {
            continue;
        }
        component.push(node);
        let edges = &adjacency[&node];
        if edges.len() != 2 {
            all_degree_two = false;
        }
        for &(a, b) in edges {
            let next = if a == node { b } else { a };
            if !visited.contains_key(&next) {
                stack.push(next);
            }
        }
    }
    if !all_degree_two {
        return None;
    }

    // Zyklus ablaufen: bei Grad 2 ist der Weg eindeutig
    let mut loop_nodes = vec![start];
    let mut prev = start;
    let mut current = {
        let (a, b) = adjacency[&start][0];
        if a == start {
            b
        } else {
            a
        }
    };
    while current != start {
        loop_nodes.push(current);
        let next = adjacency[&current]
            .iter()
            .map(|&(a, b)| if a == current { b } else { a })
            .find(|&n| n != prev)?;
        prev = current;
        current = next;
    }

    // Einfacher Zyklus ⇔ Schleife deckt die ganze Komponente ab
    // (sonst z. B. zwei getrennte Zyklen, die hier nicht auftreten können,
    //  da Grad 2 überall gilt und die Komponente zusammenhängt)
    (loop_nodes.len() == component.len()).then_some(loop_nodes)
}

/// Shoelace-Formel; positiv ⇔ gegen den Uhrzeigersinn.
fn signed_area(points: &[[f64; 2]]) -> f64 {
    let mut area = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        area += a[0] * b[1] - b[0] * a[1];
    }
    area * 0.5
}

#[derive(Default)]
struct UnionFind {
    parent: HashMap<PointId, PointId>,
}

impl UnionFind {
    fn find(&mut self, id: PointId) -> PointId {
        let parent = *self.parent.get(&id).unwrap_or(&id);
        if parent == id {
            return id;
        }
        let root = self.find(parent);
        self.parent.insert(id, root);
        root
    }

    fn union(&mut self, a: PointId, b: PointId) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent.insert(ra, rb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(sketch: &mut Sketch) -> Vec<PointId> {
        let pts = vec![
            sketch.add_point([0.0, 0.0]),
            sketch.add_point([10.0, 0.0]),
            sketch.add_point([10.0, 10.0]),
            sketch.add_point([0.0, 10.0]),
        ];
        for i in 0..4 {
            sketch.add_line(pts[i], pts[(i + 1) % 4]);
        }
        pts
    }

    #[test]
    fn chained_square_is_one_ccw_polygon() {
        let mut s = Sketch::new();
        square(&mut s);
        let profiles = s.find_profiles();
        assert_eq!(profiles.len(), 1);
        let Profile::Polygon(points) = &profiles[0] else {
            panic!("Polygon erwartet");
        };
        assert_eq!(points.len(), 4);
        assert!(signed_area(points) > 0.0, "CCW erwartet");
    }

    #[test]
    fn clockwise_drawing_is_normalized_to_ccw() {
        let mut s = Sketch::new();
        let pts = [
            s.add_point([0.0, 0.0]),
            s.add_point([0.0, 10.0]),
            s.add_point([10.0, 10.0]),
            s.add_point([10.0, 0.0]),
        ];
        for i in 0..4 {
            s.add_line(pts[i], pts[(i + 1) % 4]);
        }
        let profiles = s.find_profiles();
        let Profile::Polygon(points) = &profiles[0] else {
            panic!("Polygon erwartet");
        };
        assert!(signed_area(points) > 0.0);
    }

    #[test]
    fn open_chain_and_branch_are_no_profile() {
        let mut s = Sketch::new();
        let a = s.add_point([0.0, 0.0]);
        let b = s.add_point([5.0, 0.0]);
        let c = s.add_point([5.0, 5.0]);
        s.add_line(a, b);
        s.add_line(b, c);
        assert!(s.find_profiles().is_empty());

        // T-Verzweigung am geschlossenen Quadrat → ebenfalls kein Profil
        let mut s2 = Sketch::new();
        let pts = square(&mut s2);
        let extra = s2.add_point([20.0, 0.0]);
        s2.add_line(pts[1], extra);
        assert!(s2.find_profiles().is_empty());
    }

    #[test]
    fn circle_is_a_profile() {
        let mut s = Sketch::new();
        let c = s.add_point([3.0, 4.0]);
        s.add_circle(c, 2.5);
        assert_eq!(
            s.find_profiles(),
            vec![Profile::Circle {
                center: [3.0, 4.0],
                radius: 2.5
            }]
        );
    }

    #[test]
    fn coincident_constraint_closes_loop() {
        // Dreieck aus drei losen Linien, deren Enden per Koinzidenz
        // verschmolzen sind (nach solve liegen sie aufeinander)
        let mut s = Sketch::new();
        let a1 = s.add_point([0.0, 0.0]);
        let b1 = s.add_point([10.0, 0.0]);
        let b2 = s.add_point([10.0, 0.1]);
        let c1 = s.add_point([5.0, 8.0]);
        let c2 = s.add_point([5.1, 8.0]);
        let a2 = s.add_point([0.1, 0.0]);
        s.add_line(a1, b1);
        s.add_line(b2, c1);
        s.add_line(c2, a2);
        s.add_constraint(Constraint::Coincident(b1, b2)).unwrap();
        s.add_constraint(Constraint::Coincident(c1, c2)).unwrap();
        s.add_constraint(Constraint::Coincident(a2, a1)).unwrap();
        s.solve();

        let profiles = s.find_profiles();
        assert_eq!(profiles.len(), 1);
        let Profile::Polygon(points) = &profiles[0] else {
            panic!("Polygon erwartet");
        };
        assert_eq!(points.len(), 3);
    }
}
