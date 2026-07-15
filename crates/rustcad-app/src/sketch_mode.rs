use egui::{Color32, FontId, Pos2, Rect, Stroke, Vec2};
use glam::{Mat4, Vec3};
use rustcad_core::{FeatureId, SketchPlane};
use rustcad_sketch::{
    Constraint, DimensionId, DimensionKind, DimensionTarget, EntityId, PointId, Sketch,
    SketchEntity, SolveResult,
};

use crate::camera::OrbitCamera;

const SNAP_RADIUS_PX: f32 = 10.0;
const SELECT_TOL_PX: f32 = 6.0;
const CIRCLE_SEGMENTS: usize = 48;

/// Nachkommastellen der Bemaßungsanzeige. Zentral gehalten und für
/// spätere Einheiten (mm/inch) vorbereitet — siehe [`format_dimension`].
const DIM_DECIMALS: usize = 2;
/// Schriftgröße der Bemaßungs-Labels (Screen-Space, zoom-unabhängig).
const DIM_FONT_PX: f32 = 13.0;
/// Länge der Pfeilspitzen in Pixeln (Screen-Space).
const DIM_ARROW_PX: f32 = 9.0;

const COLOR_ENTITY: Color32 = Color32::from_rgb(120, 175, 255);
const COLOR_SELECTED: Color32 = Color32::from_rgb(255, 160, 60);
const COLOR_POINT: Color32 = Color32::from_rgb(190, 215, 255);
const COLOR_PREVIEW: Color32 = Color32::from_rgb(150, 150, 160);
const COLOR_SNAP: Color32 = Color32::from_rgb(250, 220, 90);
const COLOR_COMPLETED: Color32 = Color32::from_rgb(95, 115, 150);
const COLOR_DIM: Color32 = Color32::from_rgb(210, 170, 120);
const COLOR_DIM_HOVER: Color32 = Color32::from_rgb(245, 205, 140);
const COLOR_DIM_LABEL_BG: Color32 = Color32::from_rgba_premultiplied(20, 22, 28, 200);

/// Zentrale Zahlenformatierung für Bemaßungen (feste Nachkommastellen).
/// Einheiten folgen später — deshalb hier gebündelt.
pub fn format_dimension(value: f64) -> String {
    format!("{value:.DIM_DECIMALS$}")
}

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

/// Ein selektierbares Element: Punkt, Entity oder Bemaßung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected {
    Point(PointId),
    Entity(EntityId),
    Dimension(DimensionId),
}

/// Eine über die Toolbar auslösbare Aktion für die aktuelle Selektion:
/// entweder ein reiner geometrischer Constraint oder eine Bemaßung
/// (die intern ihren treibenden Constraint mit anlegt).
#[derive(Debug, Clone, Copy)]
pub enum SketchAction {
    /// Reiner geometrischer Constraint (horizontal, parallel, …).
    Constraint(Constraint),
    /// Bemaßung: Ziel + aktueller Anzeigewert.
    Dimension(DimensionTarget, f64),
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
    /// Läuft ein Label-Drag einer Bemaßung? Ändert nur den Offset,
    /// nie die Geometrie (der Solver wird nicht angestoßen).
    dragging_dim: Option<DimensionId>,
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
            dragging_dim: None,
            saved_camera,
        }
    }

    /// Aktionen, die zur aktuellen Selektion passen (Label + Aktion).
    /// Bemaßungen erscheinen für Abstand/Radius/Durchmesser, reine
    /// Constraints für die geometrischen Beziehungen.
    pub fn available_actions(&self) -> Vec<(&'static str, SketchAction)> {
        use SketchAction::{Constraint as C, Dimension as D};
        let is_line =
            |id: EntityId| matches!(self.sketch.entity(id), Some(SketchEntity::Line { .. }));
        match *self.selection.as_slice() {
            [Selected::Entity(e)] if is_line(e) => vec![
                ("Horizontal", C(Constraint::Horizontal(e))),
                ("Vertikal", C(Constraint::Vertical(e))),
            ],
            [Selected::Entity(e)] => match self.sketch.circle_radius(e) {
                Some(r) => vec![
                    ("⌀ bemaßen", D(DimensionTarget::Diameter(e), 2.0 * r)),
                    ("R bemaßen", D(DimensionTarget::Radius(e), r)),
                ],
                None => Vec::new(),
            },
            [Selected::Entity(a), Selected::Entity(b)] if is_line(a) && is_line(b) => vec![
                ("Parallel", C(Constraint::Parallel(a, b))),
                ("Senkrecht", C(Constraint::Perpendicular(a, b))),
                ("Gleich lang", C(Constraint::Equal(a, b))),
            ],
            [Selected::Point(p), Selected::Point(q)] => {
                let (a, b) = (self.sketch.point_pos(p), self.sketch.point_pos(q));
                let d = dist2(a, b).sqrt();
                vec![
                    ("Koinzident", C(Constraint::Coincident(p, q))),
                    ("Abstand bemaßen", D(DimensionTarget::Linear(p, q), d)),
                ]
            }
            _ => Vec::new(),
        }
    }

    /// Wendet eine Toolbar-Aktion an und löst sofort. Bemaßungen legen
    /// ihren treibenden Constraint mit an und bekommen einen sinnvollen
    /// Start-Offset, damit das Label neben der Geometrie sitzt.
    pub fn apply_action(&mut self, action: SketchAction) {
        let ok = match action {
            SketchAction::Constraint(c) => self.sketch.add_constraint(c).is_ok(),
            SketchAction::Dimension(target, value) => {
                let offset = self.default_dim_offset(target);
                self.sketch.add_dimension(target, value, offset).is_ok()
            }
        };
        if ok {
            self.last_solve = Some(self.sketch.solve());
            self.selection.clear();
        }
    }

    /// Sinnvoller Start-Offset für eine neue Bemaßung, in Ebenen-Einheiten
    /// relativ zur Referenz (Segmentmitte bzw. Kreismittelpunkt).
    fn default_dim_offset(&self, target: DimensionTarget) -> [f64; 2] {
        match target {
            DimensionTarget::Linear(p, q) => {
                let a = self.sketch.point_pos(p);
                let b = self.sketch.point_pos(q);
                let len = dist2(a, b).sqrt().max(1e-6);
                // Senkrecht zum Segment, ein Fünftel der Länge nach außen.
                let n = [-(b[1] - a[1]) / len, (b[0] - a[0]) / len];
                let d = (len * 0.2).max(0.5);
                [n[0] * d, n[1] * d]
            }
            DimensionTarget::Radius(e) | DimensionTarget::Diameter(e) => {
                let r = self.sketch.circle_radius(e).unwrap_or(1.0).max(1e-6);
                let k = std::f64::consts::FRAC_1_SQRT_2 * r * 1.6;
                [k, k]
            }
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
        let mouse_screen = response.interact_pointer_pos().or_else(|| response.hover_pos());
        let mouse_plane = mouse_screen.map(|p| map.screen_to_plane(p));
        // Welche Bemaßung liegt unter dem Cursor? (Hover + Klick + Label-Drag)
        let dim_hit = mouse_screen.and_then(|p| self.dimension_at_screen(&map, p));

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
                let point = mouse_plane.and_then(|pos| {
                    self.sketch
                        .nearest_point(pos, map.px_to_plane(SNAP_RADIUS_PX))
                        .map(|(id, _)| id)
                });
                if point.is_some() {
                    self.dragging = point;
                } else {
                    // Kein Punkt-Handle: ggf. ein Bemaßungs-Label greifen.
                    self.dragging_dim = dim_hit;
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let (Some(point), Some(pos)) = (self.dragging, mouse_plane) {
                    self.last_solve = Some(self.sketch.solve_drag(point, pos));
                } else if let (Some(dim), Some(pos)) = (self.dragging_dim, mouse_plane) {
                    // Label-Drag ändert ausschließlich den Offset — kein
                    // Solver-Lauf, keine Geometrieänderung.
                    if let Some(r) = self.dim_reference(dim) {
                        self.sketch
                            .set_dimension_offset(dim, [pos[0] - r[0], pos[1] - r[1]]);
                    }
                }
            }
            if response.drag_stopped() {
                if self.dragging.take().is_some() {
                    // Drag-Constraint fällt weg; harte Constraints exakt nachziehen
                    self.last_solve = Some(self.sketch.solve());
                }
                self.dragging_dim = None;
            }
        }

        if let Some(pos) = mouse_plane {
            if response.clicked() {
                let shift = ui.input(|i| i.modifiers.shift);
                self.on_click(pos, snap, dim_hit, map.px_to_plane(SELECT_TOL_PX), shift);
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
                    match item {
                        Selected::Entity(id) => self.sketch.delete_entity(id),
                        // Bemaßung löschen entfernt auch ihren treibenden
                        // Constraint (Kaskade in rustcad-sketch).
                        Selected::Dimension(id) => self.sketch.remove_dimension(id),
                        Selected::Point(_) => {}
                    }
                }
            }
        });

        self.paint(ui, rect, &map, mouse_plane, snap, dim_hit);
    }

    fn on_click(
        &mut self,
        mouse: [f64; 2],
        snap: Option<(PointId, [f64; 2])>,
        dim_hit: Option<DimensionId>,
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
                // Priorität: Punkte > Bemaßungen > Entities
                let hit = self
                    .sketch
                    .nearest_point(mouse, select_tol)
                    .map(|(id, _)| Selected::Point(id))
                    .or(dim_hit.map(Selected::Dimension))
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
        dim_hit: Option<DimensionId>,
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

        // Bemaßungen (über den Entities, unter den Punkt-Handles)
        self.paint_dimensions(&painter, map, dim_hit);

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

    /// Zeichnet alle Bemaßungen als klassische technische Maße im
    /// Screen-Space (Strichstärke/Textgröße zoom-unabhängig).
    fn paint_dimensions(&self, painter: &egui::Painter, map: &PlaneMap, dim_hit: Option<DimensionId>) {
        for (id, _) in self.sketch.dimensions() {
            let Some(geom) = self.dim_screen_geom(map, id) else {
                continue;
            };
            let color = if self.selection.contains(&Selected::Dimension(id)) {
                COLOR_SELECTED
            } else if dim_hit == Some(id) {
                COLOR_DIM_HOVER
            } else {
                COLOR_DIM
            };
            let stroke = Stroke::new(1.4, color);
            for seg in &geom.lines {
                painter.line_segment(*seg, stroke);
            }
            for &(tip, dir) in &geom.arrows {
                draw_arrowhead(painter, tip, dir, color);
            }
            // Wert-Label mit dezentem Hintergrund für Lesbarkeit.
            let galley =
                painter.layout_no_wrap(geom.text, FontId::proportional(DIM_FONT_PX), color);
            let bg = Rect::from_center_size(geom.label, galley.size() + egui::vec2(6.0, 2.0));
            painter.rect_filled(bg, 3.0, COLOR_DIM_LABEL_BG);
            painter.galley(geom.label - galley.size() * 0.5, galley, color);
        }
    }

    /// Screen-Space-Geometrie einer Bemaßung (Linien, Pfeile, Label).
    /// `None`, falls der treibende Constraint/Referenzen ungültig sind.
    fn dim_screen_geom(&self, map: &PlaneMap, id: DimensionId) -> Option<DimGeom> {
        let dim = self.sketch.dimension(id)?;
        let value = self.sketch.dimension_value(id)?;
        let o = dim.offset;
        match *self.sketch.constraint(dim.constraint)? {
            Constraint::Distance(p, q, _) => {
                let a = self.sketch.point_pos(p);
                let b = self.sketch.point_pos(q);
                let sa = map.plane_to_screen(a);
                let sb = map.plane_to_screen(b);
                let da = map.plane_to_screen([a[0] + o[0], a[1] + o[1]]);
                let db = map.plane_to_screen([b[0] + o[0], b[1] + o[1]]);
                let dir = safe_dir(db - da);
                let perp = egui::vec2(-dir.y, dir.x);
                let mid = da + (db - da) * 0.5;
                Some(DimGeom {
                    // Maßlinie + zwei Maßhilfslinien zu den Endpunkten.
                    lines: vec![[sa, da], [sb, db], [da, db]],
                    arrows: vec![(da, -dir), (db, dir)],
                    label: mid + perp * (DIM_FONT_PX * 0.5 + 3.0),
                    text: format_dimension(value),
                })
            }
            Constraint::Radius(e, _) => {
                let center = match self.sketch.entity(e)? {
                    SketchEntity::Circle { center, .. } => self.sketch.point_pos(*center),
                    _ => return None,
                };
                let r = self.sketch.circle_radius(e)?;
                let olen = (o[0] * o[0] + o[1] * o[1]).sqrt();
                let d = if olen > 1e-9 {
                    [o[0] / olen, o[1] / olen]
                } else {
                    let k = std::f64::consts::FRAC_1_SQRT_2;
                    [k, k]
                };
                let edge = map.plane_to_screen([center[0] + r * d[0], center[1] + r * d[1]]);
                let label = map.plane_to_screen([center[0] + o[0], center[1] + o[1]]);
                let sc = map.plane_to_screen(center);
                let prefix = match dim.kind {
                    DimensionKind::Diameter => "⌀",
                    _ => "R",
                };
                Some(DimGeom {
                    lines: vec![[edge, label]],
                    arrows: vec![(edge, safe_dir(sc - edge))],
                    label,
                    text: format!("{prefix}{}", format_dimension(value)),
                })
            }
            _ => None,
        }
    }

    /// Referenzpunkt einer Bemaßung in Ebenen-Koordinaten (Segmentmitte
    /// bzw. Kreismittelpunkt) — Basis für den relativen Label-Offset.
    fn dim_reference(&self, id: DimensionId) -> Option<[f64; 2]> {
        let dim = self.sketch.dimension(id)?;
        match *self.sketch.constraint(dim.constraint)? {
            Constraint::Distance(p, q, _) => {
                let a = self.sketch.point_pos(p);
                let b = self.sketch.point_pos(q);
                Some([(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5])
            }
            Constraint::Radius(e, _) => match self.sketch.entity(e)? {
                SketchEntity::Circle { center, .. } => Some(self.sketch.point_pos(*center)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Bemaßung unter dem Bildschirm-Cursor (Label-Box oder Maßlinie
    /// innerhalb der Selektionstoleranz).
    fn dimension_at_screen(&self, map: &PlaneMap, cursor: Pos2) -> Option<DimensionId> {
        let mut best: Option<(DimensionId, f32)> = None;
        for (id, _) in self.sketch.dimensions() {
            let Some(geom) = self.dim_screen_geom(map, id) else {
                continue;
            };
            // Label-Box (grob aus Zeichenzahl geschätzt).
            let half = egui::vec2(geom.text.chars().count() as f32 * 3.8 + 4.0, DIM_FONT_PX * 0.7);
            let label_rect = Rect::from_center_size(geom.label, half * 2.0);
            let mut d = if label_rect.contains(cursor) { 0.0 } else { f32::INFINITY };
            for seg in &geom.lines {
                d = d.min(dist_point_segment_px(cursor, seg[0], seg[1]));
            }
            if d <= SELECT_TOL_PX && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((id, d));
            }
        }
        best.map(|(id, _)| id)
    }
}

/// Screen-Space-Darstellung einer Bemaßung.
struct DimGeom {
    /// Zu zeichnende Linien (Maßlinie, Maßhilfslinien / Führungslinie).
    lines: Vec<[Pos2; 2]>,
    /// Pfeilspitzen: `(Spitze, Richtung in die die Spitze zeigt)`.
    arrows: Vec<(Pos2, Vec2)>,
    /// Bildschirmposition des Wert-Labels (Zentrum).
    label: Pos2,
    /// Beschrifteter Wert inkl. Präfix (`R`, `⌀`).
    text: String,
}

/// Zeichnet eine Pfeilspitze am `tip`, die in `dir` zeigt.
fn draw_arrowhead(painter: &egui::Painter, tip: Pos2, dir: Vec2, color: Color32) {
    let back = tip - dir * DIM_ARROW_PX;
    let perp = egui::vec2(-dir.y, dir.x) * (DIM_ARROW_PX * 0.4);
    let stroke = Stroke::new(1.4, color);
    painter.line_segment([tip, back + perp], stroke);
    painter.line_segment([tip, back - perp], stroke);
}

/// Normierte Richtung; Nullvektor → `(1, 0)` (degenerierter Fall).
fn safe_dir(v: Vec2) -> Vec2 {
    let len = v.length();
    if len > 1e-6 {
        v / len
    } else {
        Vec2::new(1.0, 0.0)
    }
}

/// Abstand Punkt→Strecke in Bildschirm-Pixeln.
fn dist_point_segment_px(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    let t = if len_sq <= f32::EPSILON {
        0.0
    } else {
        ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0)
    };
    (p - (a + ab * t)).length()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Rect {
        Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))
    }

    /// Session mit einer Linie (0,0)-(10,0) und einer Längenbemaßung.
    fn linear_session() -> (SketchSession, OrbitCamera, DimensionId) {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let p1 = session.sketch.add_point([0.0, 0.0]);
        let p2 = session.sketch.add_point([10.0, 0.0]);
        session.sketch.add_line(p1, p2);
        let dim = session
            .sketch
            .add_dimension(DimensionTarget::Linear(p1, p2), 10.0, [0.0, -2.0])
            .expect("dimension");
        (session, camera, dim)
    }

    #[test]
    fn format_dimension_uses_fixed_decimals() {
        assert_eq!(format_dimension(15.0), "15.00");
        assert_eq!(format_dimension(12.3456), "12.35");
    }

    #[test]
    fn linear_reference_is_segment_midpoint() {
        let (session, _camera, dim) = linear_session();
        assert_eq!(session.dim_reference(dim), Some([5.0, 0.0]));
    }

    #[test]
    fn radius_reference_is_circle_center() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let c = session.sketch.add_point([2.0, 3.0]);
        let circle = session.sketch.add_circle(c, 1.0);
        let dim = session
            .sketch
            .add_dimension(DimensionTarget::Radius(circle), 1.0, [1.0, 1.0])
            .expect("dimension");
        assert_eq!(session.dim_reference(dim), Some([2.0, 3.0]));
    }

    /// Akzeptanz: Label-Drag ändert nie die Geometrie.
    #[test]
    fn label_offset_change_leaves_geometry_untouched() {
        let (mut session, _camera, dim) = linear_session();
        let before: Vec<_> = session.sketch.points().map(|(_, p)| p).collect();
        // So verschiebt handle_viewport das Label (relativ zur Referenz).
        let reference = session.dim_reference(dim).unwrap();
        let target = [7.0, 9.0];
        session.sketch.set_dimension_offset(
            dim,
            [target[0] - reference[0], target[1] - reference[1]],
        );
        let after: Vec<_> = session.sketch.points().map(|(_, p)| p).collect();
        assert_eq!(before, after);
        assert_eq!(session.sketch.dimension(dim).unwrap().offset, [2.0, 9.0]);
    }

    /// Hit-Test findet die Bemaßung an ihrer Label-Position wieder.
    #[test]
    fn hit_test_finds_dimension_under_label() {
        let (session, camera, dim) = linear_session();
        let map = PlaneMap::new(SketchPlane::XY, &camera, viewport());
        let geom = session.dim_screen_geom(&map, dim).expect("geometry");
        assert_eq!(session.dimension_at_screen(&map, geom.label), Some(dim));
        // Weit entfernter Punkt trifft nichts.
        assert_eq!(
            session.dimension_at_screen(&map, Pos2::new(5.0, 5.0)),
            None
        );
    }

    /// Diameter-Label zeigt den doppelten Radius mit ⌀-Präfix.
    #[test]
    fn diameter_label_shows_doubled_value() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let c = session.sketch.add_point([0.0, 0.0]);
        let circle = session.sketch.add_circle(c, 2.0);
        let dim = session
            .sketch
            .add_dimension(DimensionTarget::Diameter(circle), 4.0, [1.5, 1.5])
            .expect("dimension");
        let map = PlaneMap::new(SketchPlane::XY, &camera, viewport());
        let geom = session.dim_screen_geom(&map, dim).expect("geometry");
        assert_eq!(geom.text, "⌀4.00");
    }
}
