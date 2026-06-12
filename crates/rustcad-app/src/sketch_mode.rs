use egui::{Color32, Pos2, Rect, Stroke};
use glam::{Mat4, Vec3};
use rustcad_core::{FeatureId, SketchPlane};
use rustcad_sketch::{Constraint, EntityId, PointId, Sketch, SketchEntity, SolveResult};

use crate::camera::OrbitCamera;

const SNAP_RADIUS_PX: f32 = 10.0;
const SELECT_TOL_PX: f32 = 6.0;
const CIRCLE_SEGMENTS: usize = 48;

const COLOR_ENTITY: Color32 = Color32::from_rgb(120, 175, 255);
const COLOR_SELECTED: Color32 = Color32::from_rgb(255, 160, 60);
const COLOR_POINT: Color32 = Color32::from_rgb(190, 215, 255);
const COLOR_PREVIEW: Color32 = Color32::from_rgb(150, 150, 160);
const COLOR_SNAP: Color32 = Color32::from_rgb(250, 220, 90);
const COLOR_COMPLETED: Color32 = Color32::from_rgb(95, 115, 150);

/// Ebenen-Achsen `(u, v, normal)` als glam-Vektoren.
pub fn plane_axes(plane: SketchPlane) -> (Vec3, Vec3, Vec3) {
    let (u, v, n) = plane.axes();
    (to_vec3(u), to_vec3(v), to_vec3(n))
}

fn to_vec3(a: [f64; 3]) -> Vec3 {
    Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32)
}

/// 2D-Ebenen-Koordinaten -> Weltkoordinaten.
fn plane_to_world(plane: SketchPlane, p: [f64; 2]) -> Vec3 {
    let (u, v, _) = plane_axes(plane);
    u * p[0] as f32 + v * p[1] as f32
}

/// Werkzeuge im Skizzenmodus. Zwischenstände (erster Klick einer Linie,
/// Kreismittelpunkt) leben hier — committed wird erst beim Abschluss,
/// damit Escape nichts in der Skizze hinterlässt.
pub enum SketchTool {
    Select,
    Line { start: Option<PendingPoint> },
    Circle { center: Option<PendingPoint> },
}

impl SketchTool {
    pub fn label(&self) -> &'static str {
        match self {
            SketchTool::Select => "Auswählen",
            SketchTool::Line { .. } => "Linie",
            SketchTool::Circle { .. } => "Kreis",
        }
    }

    fn has_pending(&self) -> bool {
        matches!(
            self,
            SketchTool::Line { start: Some(_) } | SketchTool::Circle { center: Some(_) }
        )
    }

    fn clear_pending(&mut self) {
        match self {
            SketchTool::Select => {}
            SketchTool::Line { start } => *start = None,
            SketchTool::Circle { center } => *center = None,
        }
    }
}

/// Ein angeklickter Punkt: entweder ein existierender (Snap) oder eine
/// freie Position, die beim Commit zum neuen Punkt wird.
#[derive(Clone, Copy)]
pub struct PendingPoint {
    pos: [f64; 2],
    existing: Option<PointId>,
}

/// Ein selektierbares Element: Punkt oder Entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected {
    Point(PointId),
    Entity(EntityId),
}

/// Aktiver Skizzenmodus: Ebene, Skizze, Werkzeug, Selektion.
pub struct SketchSession {
    pub plane: SketchPlane,
    /// Beim Editieren einer bestehenden Skizze: deren Feature-ID.
    pub editing: Option<FeatureId>,
    pub sketch: Sketch,
    pub tool: SketchTool,
    /// Multi-Selektion (Shift+Klick erweitert).
    pub selection: Vec<Selected>,
    /// Ergebnis des letzten Solver-Laufs (für die Statusanzeige).
    pub last_solve: Option<SolveResult>,
    dragging: Option<PointId>,
    saved_camera: OrbitCamera,
}

impl SketchSession {
    /// Startet den Skizzenmodus und lockt die Kamera senkrecht auf die
    /// Ebene (Target wird auf die Ebene projiziert, Orbit ist deaktiviert).
    pub fn start(plane: SketchPlane, camera: &mut OrbitCamera) -> Self {
        Self::start_with(plane, Sketch::new(), None, camera)
    }

    /// Startet den Skizzenmodus zum Editieren einer bestehenden Skizze
    /// (Doppelklick im Feature-Baum).
    pub fn start_edit(
        plane: SketchPlane,
        sketch: Sketch,
        feature: FeatureId,
        camera: &mut OrbitCamera,
    ) -> Self {
        Self::start_with(plane, sketch, Some(feature), camera)
    }

    fn start_with(
        plane: SketchPlane,
        sketch: Sketch,
        editing: Option<FeatureId>,
        camera: &mut OrbitCamera,
    ) -> Self {
        let saved_camera = camera.clone();
        let (_, _, n) = plane_axes(plane);
        camera.target -= n * camera.target.dot(n);
        Self {
            plane,
            editing,
            sketch,
            tool: SketchTool::Select,
            selection: Vec::new(),
            last_solve: None,
            dragging: None,
            saved_camera,
        }
    }

    /// Constraints, die zur aktuellen Selektion passen
    /// (Label + fertig parametrisierter Constraint).
    pub fn available_constraints(&self) -> Vec<(&'static str, Constraint)> {
        let is_line =
            |id: EntityId| matches!(self.sketch.entity(id), Some(SketchEntity::Line { .. }));
        match *self.selection.as_slice() {
            [Selected::Entity(e)] if is_line(e) => vec![
                ("Horizontal", Constraint::Horizontal(e)),
                ("Vertikal", Constraint::Vertical(e)),
            ],
            [Selected::Entity(e)] => match self.sketch.circle_radius(e) {
                Some(r) => vec![("Radius fixieren", Constraint::Radius(e, r))],
                None => Vec::new(),
            },
            [Selected::Entity(a), Selected::Entity(b)] if is_line(a) && is_line(b) => vec![
                ("Parallel", Constraint::Parallel(a, b)),
                ("Senkrecht", Constraint::Perpendicular(a, b)),
                ("Gleich lang", Constraint::Equal(a, b)),
            ],
            [Selected::Point(p), Selected::Point(q)] => {
                let (a, b) = (self.sketch.point_pos(p), self.sketch.point_pos(q));
                let d = dist2(a, b).sqrt();
                vec![
                    ("Koinzident", Constraint::Coincident(p, q)),
                    ("Abstand fixieren", Constraint::Distance(p, q, d)),
                ]
            }
            _ => Vec::new(),
        }
    }

    /// Fügt einen Constraint hinzu und löst sofort.
    pub fn apply_constraint(&mut self, c: Constraint) {
        if self.sketch.add_constraint(c).is_ok() {
            self.last_solve = Some(self.sketch.solve());
            self.selection.clear();
        }
    }

    /// Beendet den Skizzenmodus, stellt die Kamera wieder her und gibt
    /// Ebene + Skizze zurück (plus Feature-ID beim Editieren).
    pub fn finish(self, camera: &mut OrbitCamera) -> (Option<FeatureId>, SketchPlane, Sketch) {
        *camera = self.saved_camera;
        (self.editing, self.plane, self.sketch)
    }

    /// View-Projection der auf die Ebene gelockten Kamera.
    pub fn view_proj(&self, camera: &OrbitCamera, aspect: f32) -> Mat4 {
        let (_, v, n) = plane_axes(self.plane);
        let eye = camera.target + n * camera.distance;
        camera.proj(aspect) * Mat4::look_at_rh(eye, camera.target, v)
    }

    /// Blickrichtung im Skizzenmodus (für das Headlight).
    pub fn forward(&self) -> Vec3 {
        -plane_axes(self.plane).2
    }

    /// Verarbeitet Viewport-Eingaben und zeichnet das Skizzen-Overlay.
    pub fn handle_viewport(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        response: &egui::Response,
        camera: &mut OrbitCamera,
    ) {
        let (u, v, _) = plane_axes(self.plane);

        // Kamera: Pan in der Ebene + Zoom; Orbit ist gelockt
        if response.dragged_by(egui::PointerButton::Middle) {
            camera.pan_along(response.drag_delta(), rect.height(), u, v);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                camera.zoom(scroll);
            }
        }

        let map = PlaneMap::new(self.plane, camera, rect);
        let mouse_plane = response
            .interact_pointer_pos()
            .or_else(|| response.hover_pos())
            .map(|p| map.screen_to_plane(p));

        // Snapping auf existierende Endpunkte (nur in Zeichenwerkzeugen)
        let snap = match (&self.tool, mouse_plane) {
            (SketchTool::Select, _) | (_, None) => None,
            (_, Some(pos)) => self
                .sketch
                .nearest_point(pos, map.px_to_plane(SNAP_RADIUS_PX)),
        };

        // Live-Drag: Punkt greifen und ziehen — der Solver läuft pro Frame
        // mit der Mausposition als temporärem, weichem Constraint
        if matches!(self.tool, SketchTool::Select) {
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(pos) = mouse_plane {
                    self.dragging = self
                        .sketch
                        .nearest_point(pos, map.px_to_plane(SNAP_RADIUS_PX))
                        .map(|(id, _)| id);
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let (Some(point), Some(pos)) = (self.dragging, mouse_plane) {
                    self.last_solve = Some(self.sketch.solve_drag(point, pos));
                }
            }
            if response.drag_stopped() && self.dragging.take().is_some() {
                // Drag-Constraint fällt weg; harte Constraints exakt nachziehen
                self.last_solve = Some(self.sketch.solve());
            }
        }

        if let Some(pos) = mouse_plane {
            if response.clicked() {
                let shift = ui.input(|i| i.modifiers.shift);
                self.on_click(pos, snap, map.px_to_plane(SELECT_TOL_PX), shift);
            }
        }

        ui.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                if self.tool.has_pending() {
                    self.tool.clear_pending();
                } else {
                    self.tool = SketchTool::Select;
                    self.selection.clear();
                }
            }
            if i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace) {
                for item in std::mem::take(&mut self.selection) {
                    if let Selected::Entity(id) = item {
                        self.sketch.delete_entity(id);
                    }
                }
            }
        });

        self.paint(ui, rect, &map, mouse_plane, snap);
    }

    fn on_click(
        &mut self,
        mouse: [f64; 2],
        snap: Option<(PointId, [f64; 2])>,
        select_tol: f64,
        shift: bool,
    ) {
        let effective = match snap {
            Some((id, pos)) => PendingPoint {
                pos,
                existing: Some(id),
            },
            None => PendingPoint {
                pos: mouse,
                existing: None,
            },
        };

        match &mut self.tool {
            SketchTool::Select => {
                // Punkte haben Vorrang vor Entities
                let hit = self
                    .sketch
                    .nearest_point(mouse, select_tol)
                    .map(|(id, _)| Selected::Point(id))
                    .or_else(|| {
                        self.sketch
                            .hit_test(mouse, select_tol)
                            .map(Selected::Entity)
                    });
                match (hit, shift) {
                    (Some(item), true) => {
                        if let Some(i) = self.selection.iter().position(|s| *s == item) {
                            self.selection.remove(i);
                        } else {
                            self.selection.push(item);
                        }
                    }
                    (Some(item), false) => self.selection = vec![item],
                    (None, false) => self.selection.clear(),
                    (None, true) => {}
                }
            }
            SketchTool::Line { start } => match start.take() {
                None => *start = Some(effective),
                Some(s) => {
                    if dist2(s.pos, effective.pos) > 1e-12 {
                        let p1 = resolve_point(&mut self.sketch, s);
                        let p2 = resolve_point(&mut self.sketch, effective);
                        self.sketch.add_line(p1, p2);
                        // Kettenmodus: Endpunkt wird Start der nächsten Linie
                        *start = Some(PendingPoint {
                            pos: effective.pos,
                            existing: Some(p2),
                        });
                    } else {
                        *start = Some(s);
                    }
                }
            },
            SketchTool::Circle { center } => match center.take() {
                None => *center = Some(effective),
                Some(c) => {
                    let radius = dist2(c.pos, mouse).sqrt();
                    if radius > 1e-6 {
                        let center_id = resolve_point(&mut self.sketch, c);
                        self.sketch.add_circle(center_id, radius);
                    } else {
                        *center = Some(c);
                    }
                }
            },
        }
    }

    fn paint(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        map: &PlaneMap,
        mouse_plane: Option<[f64; 2]>,
        snap: Option<(PointId, [f64; 2])>,
    ) {
        let painter = ui.painter_at(rect);

        // Entities
        for (id, entity) in self.sketch.entities() {
            let color = if self.selection.contains(&Selected::Entity(id)) {
                COLOR_SELECTED
            } else {
                COLOR_ENTITY
            };
            let stroke = Stroke::new(1.6, color);
            match *entity {
                SketchEntity::Line { p1, p2 } => {
                    painter.line_segment(
                        [
                            map.plane_to_screen(self.sketch.point_pos(p1)),
                            map.plane_to_screen(self.sketch.point_pos(p2)),
                        ],
                        stroke,
                    );
                }
                SketchEntity::Circle { center, .. } => {
                    let radius = self.sketch.circle_radius(id).unwrap_or(0.0);
                    painter.circle_stroke(
                        map.plane_to_screen(self.sketch.point_pos(center)),
                        map.plane_to_px(radius),
                        stroke,
                    );
                }
            }
        }

        // Punkte (Endpunkte/Mittelpunkte als Handles)
        for (id, pos) in self.sketch.points() {
            let (radius, color) = if self.selection.contains(&Selected::Point(id)) {
                (4.5, COLOR_SELECTED)
            } else {
                (3.0, COLOR_POINT)
            };
            painter.circle_filled(map.plane_to_screen(pos), radius, color);
        }

        // Werkzeug-Vorschau (Gummiband)
        if let Some(mouse) = mouse_plane {
            let target = snap.map_or(mouse, |(_, p)| p);
            let preview = Stroke::new(1.2, COLOR_PREVIEW);
            match &self.tool {
                SketchTool::Line { start: Some(s) } => {
                    painter.extend(egui::Shape::dashed_line(
                        &[map.plane_to_screen(s.pos), map.plane_to_screen(target)],
                        preview,
                        6.0,
                        4.0,
                    ));
                }
                SketchTool::Circle { center: Some(c) } => {
                    let radius = dist2(c.pos, mouse).sqrt();
                    painter.circle_stroke(
                        map.plane_to_screen(c.pos),
                        map.plane_to_px(radius),
                        preview,
                    );
                    painter.extend(egui::Shape::dashed_line(
                        &[map.plane_to_screen(c.pos), map.plane_to_screen(mouse)],
                        preview,
                        6.0,
                        4.0,
                    ));
                }
                _ => {}
            }
        }

        // Snap-Marker
        if let Some((_, pos)) = snap {
            painter.circle_stroke(map.plane_to_screen(pos), 7.0, Stroke::new(1.5, COLOR_SNAP));
        }
    }
}

fn resolve_point(sketch: &mut Sketch, pending: PendingPoint) -> PointId {
    pending
        .existing
        .unwrap_or_else(|| sketch.add_point(pending.pos))
}

fn dist2(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Abbildung Bildschirm ↔ Ebenen-Koordinaten bei senkrecht gelockter
/// Kamera: Bildschirm-Rechts = u, Bildschirm-Hoch = v.
struct PlaneMap {
    center: Pos2,
    target_uv: [f32; 2],
    world_per_px: f32,
}

impl PlaneMap {
    fn new(plane: SketchPlane, camera: &OrbitCamera, rect: Rect) -> Self {
        let (u, v, _) = plane_axes(plane);
        Self {
            center: rect.center(),
            target_uv: [camera.target.dot(u), camera.target.dot(v)],
            world_per_px: camera.world_per_pixel(rect.height()),
        }
    }

    fn screen_to_plane(&self, p: Pos2) -> [f64; 2] {
        let dx = (p.x - self.center.x) * self.world_per_px;
        let dy = -(p.y - self.center.y) * self.world_per_px;
        [
            (self.target_uv[0] + dx) as f64,
            (self.target_uv[1] + dy) as f64,
        ]
    }

    fn plane_to_screen(&self, p: [f64; 2]) -> Pos2 {
        let dx = (p[0] as f32 - self.target_uv[0]) / self.world_per_px;
        let dy = (p[1] as f32 - self.target_uv[1]) / self.world_per_px;
        self.center + egui::vec2(dx, -dy)
    }

    fn px_to_plane(&self, px: f32) -> f64 {
        (px * self.world_per_px) as f64
    }

    fn plane_to_px(&self, len: f64) -> f32 {
        len as f32 / self.world_per_px
    }
}

/// Zeichnet eine Dokument-Skizze als Welt-Overlay (beliebige
/// Kameralage): Punkte werden einzeln projiziert, Kreise als Polylinie.
pub fn paint_sketch(
    painter: &egui::Painter,
    rect: Rect,
    view_proj: Mat4,
    plane: SketchPlane,
    sketch: &Sketch,
) {
    let stroke = Stroke::new(1.2, COLOR_COMPLETED);
    let project = |p: [f64; 2]| project_to_screen(view_proj, rect, plane_to_world(plane, p));

    for (id, entity) in sketch.entities() {
        match *entity {
            SketchEntity::Line { p1, p2 } => {
                if let (Some(a), Some(b)) =
                    (project(sketch.point_pos(p1)), project(sketch.point_pos(p2)))
                {
                    painter.line_segment([a, b], stroke);
                }
            }
            SketchEntity::Circle { center, .. } => {
                let radius = sketch.circle_radius(id).unwrap_or(0.0);
                let c = sketch.point_pos(center);
                let points: Vec<Pos2> = (0..=CIRCLE_SEGMENTS)
                    .filter_map(|i| {
                        let t = i as f64 / CIRCLE_SEGMENTS as f64 * std::f64::consts::TAU;
                        project([c[0] + radius * t.cos(), c[1] + radius * t.sin()])
                    })
                    .collect();
                if points.len() > 1 {
                    painter.add(egui::Shape::line(points, stroke));
                }
            }
        }
    }
}

/// Weltpunkt → Bildschirmkoordinaten; `None` hinter der Kamera.
fn project_to_screen(view_proj: Mat4, rect: Rect, world: Vec3) -> Option<Pos2> {
    let clip = view_proj * world.extend(1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(rect.center() + egui::vec2(ndc.x * rect.width() * 0.5, -ndc.y * rect.height() * 0.5))
}
