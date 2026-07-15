use std::collections::HashMap;

use egui::{Color32, FontId, Pos2, Rect, Stroke, Vec2};
use glam::{Mat4, Vec3};
use rustcad_core::{FeatureId, SketchPlane};
use rustcad_sketch::{
    Constraint, ConstraintId, ConstraintKind, ConstraintRef, Dimension, DimensionError,
    DimensionId, DimensionKind, DimensionTarget, EntityId, PointId, Sketch, SketchEntity,
    SolveResult,
};

use crate::camera::OrbitCamera;

const SNAP_RADIUS_PX: f32 = 10.0;
const SELECT_TOL_PX: f32 = 6.0;
const CIRCLE_SEGMENTS: usize = 48;

/// Kantenlänge eines Constraint-Glyphs im Screen-Space (zoom-unabhängig).
const GLYPH_SIZE_PX: f32 = 15.0;
/// Abstand gestapelter Glyphen desselben Ankers.
const GLYPH_GAP_PX: f32 = 2.0;
/// Versatz des Glyph-Stapels vom Ankerpunkt (rechts oberhalb der Geometrie).
const GLYPH_OFFSET_PX: Vec2 = Vec2::new(11.0, -11.0);

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
const COLOR_GLYPH: Color32 = Color32::from_rgb(150, 205, 150);
const COLOR_GLYPH_HOVER: Color32 = Color32::from_rgb(130, 235, 200);
/// Highlight für die Geometrie, die ein gehoverter Glyph referenziert.
const COLOR_HIGHLIGHT: Color32 = Color32::from_rgb(130, 235, 200);

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
    /// Bemaßungswerkzeug (Zustandsautomat, siehe [`DimStage`]).
    Dimension(DimStage),
}

/// Zustandsautomat des Bemaßungswerkzeugs (TECH_SPEC §7.4, keine
/// impliziten Modi): Auswahl → Platzieren → Werteingabe.
#[derive(Clone, Debug, PartialEq)]
pub enum DimStage {
    /// Warten auf Geometrieauswahl. `first_point` ist gesetzt, wenn für
    /// eine Punkt-zu-Punkt-Bemaßung bereits ein Punkt geklickt wurde.
    Selecting { first_point: Option<PointId> },
    /// Ziel steht fest; das Label folgt der Maus, ein Klick platziert es.
    Placing { target: DimensionTarget },
    /// Label platziert; Zahleneingabe läuft (Feld direkt am Label).
    Editing {
        target: DimensionTarget,
        offset: [f64; 2],
        text: String,
        /// Beim ersten Frame den Fokus ins Eingabefeld setzen.
        focus: bool,
        /// Beim Editieren einer *bestehenden* Bemaßung (Doppelklick) deren
        /// ID; `None` beim Anlegen einer neuen.
        existing: Option<DimensionId>,
    },
}

impl SketchTool {
    /// Frischer, leerer Zustand des Bemaßungswerkzeugs.
    pub fn dimension() -> Self {
        SketchTool::Dimension(DimStage::Selecting { first_point: None })
    }

    pub fn label(&self) -> &'static str {
        match self {
            SketchTool::Select => "Auswählen",
            SketchTool::Line { .. } => "Linie",
            SketchTool::Circle { .. } => "Kreis",
            SketchTool::Dimension(_) => "Bemaßung",
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
            SketchTool::Select | SketchTool::Dimension(_) => {}
            SketchTool::Line { start } => *start = None,
            SketchTool::Circle { center } => *center = None,
        }
    }
}

/// Wechselt bei Kreis-Bemaßungen zwischen Durchmesser und Radius;
/// lineare Ziele bleiben unverändert.
fn toggle_circle_target(target: DimensionTarget) -> DimensionTarget {
    match target {
        DimensionTarget::Diameter(e) => DimensionTarget::Radius(e),
        DimensionTarget::Radius(e) => DimensionTarget::Diameter(e),
        linear => linear,
    }
}

/// Ein angeklickter Punkt: entweder ein existierender (Snap) oder eine
/// freie Position, die beim Commit zum neuen Punkt wird.
#[derive(Clone, Copy)]
pub struct PendingPoint {
    pos: [f64; 2],
    existing: Option<PointId>,
}

/// Ein selektierbares Element: Punkt, Entity, Bemaßung oder Constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected {
    Point(PointId),
    Entity(EntityId),
    Dimension(DimensionId),
    Constraint(ConstraintId),
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
    /// Letzter abgelehnter Bemaßungsversuch (Konflikt/Überbestimmung) —
    /// die UI formt daraus ihre Meldung. Wird bei Erfolg gelöscht.
    pub last_dim_error: Option<DimensionError>,
    /// Constraint-Glyphen im Overlay anzeigen (auf dichten Skizzen
    /// abschaltbar; Standard an).
    pub show_constraint_glyphs: bool,
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
            last_dim_error: None,
            show_constraint_glyphs: true,
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
    /// Start-Offset, damit das Label neben der Geometrie sitzt. Konflikte
    /// landen in [`SketchSession::last_dim_error`].
    pub fn apply_action(&mut self, action: SketchAction) {
        match action {
            SketchAction::Constraint(c) => {
                if self.sketch.add_constraint(c).is_ok() {
                    self.last_solve = Some(self.sketch.solve());
                    self.last_dim_error = None;
                    self.selection.clear();
                }
            }
            SketchAction::Dimension(target, value) => {
                let offset = self.default_dim_offset(target);
                self.commit_new_dimension(target, value, offset);
            }
        }
    }

    /// Legt eine neue Bemaßung an (mit Konfliktbehandlung). Bei Erfolg
    /// wird die Selektion geleert und der Fehler gelöscht; bei Ablehnung
    /// bleibt die Skizze unverändert und `last_dim_error` wird gesetzt.
    fn commit_new_dimension(&mut self, target: DimensionTarget, value: f64, offset: [f64; 2]) {
        match self.sketch.add_dimension(target, value, offset) {
            Ok(_) => {
                // add_dimension hat bereits konfliktfrei gelöst.
                self.last_dim_error = None;
                self.last_solve = Some(SolveResult::Solved { iterations: 0 });
                self.selection.clear();
            }
            Err(err) => self.last_dim_error = Some(err),
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

        // Constraint-Glyph bzw. Entity unter dem Cursor — nur im Auswahl-
        // Werkzeug relevant (Cross-Highlighting + Glyph-Selektion).
        let select_mode = matches!(self.tool, SketchTool::Select);
        let glyph_hit = if select_mode && self.show_constraint_glyphs {
            mouse_screen.and_then(|p| self.glyph_at_screen(&map, p))
        } else {
            None
        };
        let entity_hover = if select_mode {
            mouse_plane.and_then(|pos| self.sketch.hit_test(pos, map.px_to_plane(SELECT_TOL_PX)))
        } else {
            None
        };
        let highlight = self.hover_highlight(glyph_hit, entity_hover);

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

        // Das Bemaßungswerkzeug hat einen eigenen Zustandsautomaten und
        // verarbeitet Klicks, R (Umschalten ⌀/R), Enter/Esc selbst.
        if matches!(self.tool, SketchTool::Dimension(_)) {
            self.handle_dimension_tool(ui, response, &map, mouse_plane, map.px_to_plane(SELECT_TOL_PX));
        } else if response.double_clicked() {
            // Doppelklick auf eine Bemaßung öffnet ihr Werteingabefeld.
            if let Some(stage) = dim_hit.and_then(|id| self.begin_edit_dimension(id)) {
                self.tool = SketchTool::Dimension(stage);
            }
        } else if let Some(pos) = mouse_plane {
            if response.clicked() {
                let shift = ui.input(|i| i.modifiers.shift);
                self.on_click(
                    pos,
                    snap,
                    dim_hit,
                    glyph_hit,
                    map.px_to_plane(SELECT_TOL_PX),
                    shift,
                );
            }
        }

        ui.input(|i| {
            // Escape des Bemaßungswerkzeugs ist in handle_dimension_tool behandelt.
            if i.key_pressed(egui::Key::Escape) && !matches!(self.tool, SketchTool::Dimension(_)) {
                if self.tool.has_pending() {
                    self.tool.clear_pending();
                } else {
                    self.tool = SketchTool::Select;
                    self.selection.clear();
                }
            }
            if i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace) {
                let mut mutated = false;
                for item in std::mem::take(&mut self.selection) {
                    match item {
                        Selected::Entity(id) => {
                            self.sketch.delete_entity(id);
                            mutated = true;
                        }
                        // Bemaßung löschen entfernt auch ihren treibenden
                        // Constraint (Kaskade in rustcad-sketch).
                        Selected::Dimension(id) => {
                            self.sketch.remove_dimension(id);
                            mutated = true;
                        }
                        // Constraint löschen: treibt er eine Bemaßung, fällt
                        // diese mit weg (Kaskade); sonst wird nur die
                        // Beziehung gelöst und die Geometrie freigegeben.
                        Selected::Constraint(id) => {
                            self.sketch.delete_constraint(id);
                            mutated = true;
                        }
                        Selected::Point(_) => {}
                    }
                }
                if mutated {
                    // Neu lösen: aktualisiert die Geometrie und (über dof())
                    // die Freiheitsgrad-Anzeige.
                    self.last_solve = Some(self.sketch.solve());
                }
            }
        });

        self.paint(ui, rect, &map, mouse_plane, snap, dim_hit, &highlight);
    }

    #[allow(clippy::too_many_arguments)]
    fn on_click(
        &mut self,
        mouse: [f64; 2],
        snap: Option<(PointId, [f64; 2])>,
        dim_hit: Option<DimensionId>,
        glyph_hit: Option<ConstraintId>,
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
                // Priorität: Punkte > Constraint-Glyphen > Bemaßungen > Entities.
                // Glyphen sitzen versetzt neben der Geometrie, kollidieren also
                // kaum mit dem Entity-Treffer an gleicher Stelle.
                let hit = self
                    .sketch
                    .nearest_point(mouse, select_tol)
                    .map(|(id, _)| Selected::Point(id))
                    .or(glyph_hit.map(Selected::Constraint))
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
            // Das Bemaßungswerkzeug wird separat behandelt.
            SketchTool::Dimension(_) => {}
        }
    }

    /// Zustandsautomat des Bemaßungswerkzeugs. Verarbeitet Auswahl,
    /// Platzierung und Werteingabe; Esc verlässt jeden Zwischenzustand.
    fn handle_dimension_tool(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        map: &PlaneMap,
        mouse_plane: Option<[f64; 2]>,
        select_tol: f64,
    ) {
        let SketchTool::Dimension(stage) = &self.tool else {
            return;
        };
        let stage = stage.clone();
        let clicked = response.clicked();
        let (enter, esc, toggle) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::R),
            )
        });

        let next = match stage {
            DimStage::Selecting { first_point } => {
                if esc {
                    // Leerer Zustand: Werkzeug verlassen.
                    self.selection.clear();
                    SketchTool::Select
                } else if clicked {
                    match mouse_plane {
                        Some(pos) => {
                            SketchTool::Dimension(self.dim_pick(first_point, pos, select_tol))
                        }
                        None => SketchTool::Dimension(DimStage::Selecting { first_point }),
                    }
                } else {
                    SketchTool::Dimension(DimStage::Selecting { first_point })
                }
            }
            DimStage::Placing { target } => {
                let target = if toggle {
                    toggle_circle_target(target)
                } else {
                    target
                };
                if esc {
                    // Esc verlässt das Werkzeug aus jedem Auswahl-/Platzier-
                    // Zustand (Werteingabe hat eigene Esc-Semantik).
                    self.selection.clear();
                    SketchTool::Select
                } else if clicked {
                    match mouse_plane {
                        Some(pos) => {
                            let r = self.target_reference(target);
                            let offset = [pos[0] - r[0], pos[1] - r[1]];
                            let value = self.measured_value(target);
                            SketchTool::Dimension(DimStage::Editing {
                                target,
                                offset,
                                text: format!("{value:.4}"),
                                focus: true,
                                existing: None,
                            })
                        }
                        None => SketchTool::Dimension(DimStage::Placing { target }),
                    }
                } else {
                    SketchTool::Dimension(DimStage::Placing { target })
                }
            }
            DimStage::Editing {
                target,
                offset,
                mut text,
                focus,
                existing,
            } => {
                let measured = self.measured_value(target);
                let label_pos = {
                    let r = self.target_reference(target);
                    map.plane_to_screen([r[0] + offset[0], r[1] + offset[1]])
                };
                // Eingabefeld direkt am Label.
                let mut confirm_typed = false;
                egui::Area::new(egui::Id::new("rustcad-dim-input"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(label_pos)
                    .show(ui.ctx(), |ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut text)
                                .desired_width(70.0)
                                .font(egui::TextStyle::Monospace),
                        );
                        if focus {
                            resp.request_focus();
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            confirm_typed = true;
                        }
                    });

                // Enter bestätigt den eingegebenen Wert, Esc übernimmt den
                // aktuellen (gemessenen) Wert — beide fixieren die Bemaßung.
                let value = if confirm_typed || enter {
                    Some(text.trim().parse::<f64>().unwrap_or(measured))
                } else if esc {
                    Some(measured)
                } else {
                    None
                };

                match value {
                    Some(v) => {
                        // Bestehende Bemaßung ändern bzw. neue anlegen; bei
                        // Konflikt bleibt die Skizze unverändert.
                        let result = match existing {
                            Some(id) => self.sketch.set_dimension_value(id, v),
                            None => self.sketch.add_dimension(target, v, offset).map(|_| ()),
                        };
                        match result {
                            Ok(()) => {
                                self.last_dim_error = None;
                                self.last_solve = Some(SolveResult::Solved { iterations: 0 });
                            }
                            Err(err) => self.last_dim_error = Some(err),
                        }
                        // Nach dem Editieren einer bestehenden Bemaßung zurück
                        // zur Auswahl; nach dem Anlegen bereit für die nächste.
                        if existing.is_some() {
                            self.selection.clear();
                            SketchTool::Select
                        } else {
                            SketchTool::dimension()
                        }
                    }
                    None => SketchTool::Dimension(DimStage::Editing {
                        target,
                        offset,
                        text,
                        focus: false,
                        existing,
                    }),
                }
            }
        };
        self.tool = next;
    }

    /// Auflösung eines Auswahlklicks im Bemaßungswerkzeug. Priorität beim
    /// ersten Klick: Punkt (→ Punkt-zu-Punkt) > Kreis (→ ⌀) > Linie
    /// (→ Länge). Ungültige Klicks setzen die Auswahl sauber zurück.
    fn dim_pick(&self, first_point: Option<PointId>, pos: [f64; 2], tol: f64) -> DimStage {
        match first_point {
            Some(p) => {
                if let Some((q, _)) = self.sketch.nearest_point(pos, tol) {
                    if q != p {
                        return DimStage::Placing {
                            target: DimensionTarget::Linear(p, q),
                        };
                    }
                }
                // Kein gültiger zweiter Punkt: Auswahl zurücksetzen.
                DimStage::Selecting { first_point: None }
            }
            None => {
                if let Some((p, _)) = self.sketch.nearest_point(pos, tol) {
                    return DimStage::Selecting {
                        first_point: Some(p),
                    };
                }
                if let Some(e) = self.sketch.hit_test(pos, tol) {
                    if self.sketch.circle_radius(e).is_some() {
                        return DimStage::Placing {
                            target: DimensionTarget::Diameter(e),
                        };
                    }
                    if let Some(SketchEntity::Line { p1, p2 }) = self.sketch.entity(e) {
                        return DimStage::Placing {
                            target: DimensionTarget::Linear(*p1, *p2),
                        };
                    }
                }
                DimStage::Selecting { first_point: None }
            }
        }
    }

    /// Aktuell gemessener Anzeigewert eines Bemaßungsziels
    /// (Länge, Radius bzw. Durchmesser = 2r).
    fn measured_value(&self, target: DimensionTarget) -> f64 {
        match target {
            DimensionTarget::Linear(p, q) => {
                dist2(self.sketch.point_pos(p), self.sketch.point_pos(q)).sqrt()
            }
            DimensionTarget::Radius(e) => self.sketch.circle_radius(e).unwrap_or(0.0),
            DimensionTarget::Diameter(e) => 2.0 * self.sketch.circle_radius(e).unwrap_or(0.0),
        }
    }

    /// Referenzpunkt eines Ziels in Ebenen-Koordinaten (Segmentmitte
    /// bzw. Kreismittelpunkt) — Basis für den relativen Label-Offset.
    fn target_reference(&self, target: DimensionTarget) -> [f64; 2] {
        match target {
            DimensionTarget::Linear(p, q) => {
                let a = self.sketch.point_pos(p);
                let b = self.sketch.point_pos(q);
                [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
            }
            DimensionTarget::Radius(e) | DimensionTarget::Diameter(e) => {
                match self.sketch.entity(e) {
                    Some(SketchEntity::Circle { center, .. }) => self.sketch.point_pos(*center),
                    _ => [0.0, 0.0],
                }
            }
        }
    }

    /// Baut den Werteingabe-Zustand für eine *bestehende* Bemaßung
    /// (Doppelklick); `None`, falls die Bemaßung ungültig ist.
    fn begin_edit_dimension(&self, id: DimensionId) -> Option<DimStage> {
        let dim = self.sketch.dimension(id)?;
        let target = self.dim_target(dim)?;
        let value = self.sketch.dimension_value(id)?;
        Some(DimStage::Editing {
            target,
            offset: dim.offset,
            text: format!("{value:.4}"),
            focus: true,
            existing: Some(id),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        map: &PlaneMap,
        mouse_plane: Option<[f64; 2]>,
        snap: Option<(PointId, [f64; 2])>,
        dim_hit: Option<DimensionId>,
        highlight: &HoverHighlight,
    ) {
        let painter = ui.painter_at(rect);

        // Entities
        for (id, entity) in self.sketch.entities() {
            let color = if self.selection.contains(&Selected::Entity(id)) {
                COLOR_SELECTED
            } else if highlight.entities.contains(&id) {
                COLOR_HIGHLIGHT
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
            } else if highlight.points.contains(&id) {
                (4.5, COLOR_HIGHLIGHT)
            } else {
                (3.0, COLOR_POINT)
            };
            painter.circle_filled(map.plane_to_screen(pos), radius, color);
        }

        // Constraint-Glyphen (über allem — kleine Icons neben der Geometrie)
        self.paint_glyphs(&painter, map, highlight);

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
                // Punkt-zu-Punkt: gewählten Startpunkt markieren + Gummiband.
                SketchTool::Dimension(DimStage::Selecting {
                    first_point: Some(p),
                }) => {
                    let a = map.plane_to_screen(self.sketch.point_pos(*p));
                    painter.circle_filled(a, 4.5, COLOR_SELECTED);
                    painter.extend(egui::Shape::dashed_line(
                        &[a, map.plane_to_screen(mouse)],
                        preview,
                        6.0,
                        4.0,
                    ));
                }
                // Platzieren: Vorschau der Bemaßung folgt der Maus.
                SketchTool::Dimension(DimStage::Placing { target }) => {
                    let r = self.target_reference(*target);
                    let offset = [mouse[0] - r[0], mouse[1] - r[1]];
                    let value = self.measured_value(*target);
                    let geom = self.dim_geom_from(map, *target, offset, value);
                    draw_dim_geom(&painter, &geom, COLOR_DIM_HOVER, true);
                }
                // Werteingabe: Maßlinien anzeigen (Label deckt das Feld ab).
                SketchTool::Dimension(DimStage::Editing { target, offset, .. }) => {
                    let value = self.measured_value(*target);
                    let geom = self.dim_geom_from(map, *target, *offset, value);
                    draw_dim_geom(&painter, &geom, COLOR_DIM_HOVER, false);
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
            draw_dim_geom(painter, &geom, color, true);
        }
    }

    /// Das Bemaßungsziel einer bestehenden Bemaßung (aus ihrem treibenden
    /// Constraint plus Art), oder `None` bei ungültigen Referenzen.
    fn dim_target(&self, dim: &Dimension) -> Option<DimensionTarget> {
        match *self.sketch.constraint(dim.constraint)? {
            Constraint::Distance(p, q, _) => Some(DimensionTarget::Linear(p, q)),
            Constraint::Radius(e, _) => Some(match dim.kind {
                DimensionKind::Diameter => DimensionTarget::Diameter(e),
                _ => DimensionTarget::Radius(e),
            }),
            _ => None,
        }
    }

    /// Screen-Space-Geometrie einer bestehenden Bemaßung.
    /// `None`, falls der treibende Constraint/Referenzen ungültig sind.
    fn dim_screen_geom(&self, map: &PlaneMap, id: DimensionId) -> Option<DimGeom> {
        let dim = self.sketch.dimension(id)?;
        let target = self.dim_target(dim)?;
        let value = self.sketch.dimension_value(id)?;
        Some(self.dim_geom_from(map, target, dim.offset, value))
    }

    /// Baut die Screen-Space-Geometrie aus Ziel + Offset + Anzeigewert.
    /// Basis für bestehende Bemaßungen *und* die Live-Vorschau des
    /// Bemaßungswerkzeugs (setzt ein gültiges Ziel voraus).
    fn dim_geom_from(
        &self,
        map: &PlaneMap,
        target: DimensionTarget,
        o: [f64; 2],
        value: f64,
    ) -> DimGeom {
        match target {
            DimensionTarget::Linear(p, q) => {
                let a = self.sketch.point_pos(p);
                let b = self.sketch.point_pos(q);
                let sa = map.plane_to_screen(a);
                let sb = map.plane_to_screen(b);
                let da = map.plane_to_screen([a[0] + o[0], a[1] + o[1]]);
                let db = map.plane_to_screen([b[0] + o[0], b[1] + o[1]]);
                let dir = safe_dir(db - da);
                let perp = egui::vec2(-dir.y, dir.x);
                let mid = da + (db - da) * 0.5;
                DimGeom {
                    // Maßlinie + zwei Maßhilfslinien zu den Endpunkten.
                    lines: vec![[sa, da], [sb, db], [da, db]],
                    arrows: vec![(da, -dir), (db, dir)],
                    label: mid + perp * (DIM_FONT_PX * 0.5 + 3.0),
                    text: format_dimension(value),
                }
            }
            DimensionTarget::Radius(e) | DimensionTarget::Diameter(e) => {
                let center = match self.sketch.entity(e) {
                    Some(SketchEntity::Circle { center, .. }) => self.sketch.point_pos(*center),
                    _ => [0.0, 0.0],
                };
                let r = self.sketch.circle_radius(e).unwrap_or(0.0);
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
                let prefix = if matches!(target, DimensionTarget::Diameter(_)) {
                    "⌀"
                } else {
                    "R"
                };
                DimGeom {
                    lines: vec![[edge, label]],
                    arrows: vec![(edge, safe_dir(sc - edge))],
                    label,
                    text: format!("{prefix}{}", format_dimension(value)),
                }
            }
        }
    }

    /// Referenzpunkt einer bestehenden Bemaßung in Ebenen-Koordinaten.
    fn dim_reference(&self, id: DimensionId) -> Option<[f64; 2]> {
        let dim = self.sketch.dimension(id)?;
        Some(self.target_reference(self.dim_target(dim)?))
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

    /// Cross-Highlighting in beide Richtungen: ein gehoverter Glyph hebt die
    /// referenzierte Geometrie hervor, ein gehovertes Entity seine Glyphen.
    fn hover_highlight(
        &self,
        glyph_hit: Option<ConstraintId>,
        entity_hover: Option<EntityId>,
    ) -> HoverHighlight {
        let mut h = HoverHighlight::default();
        if let Some(id) = glyph_hit {
            h.glyphs.push(id);
            if let Some(info) = self.sketch.constraint_info(id) {
                for r in info.refs {
                    match r {
                        ConstraintRef::Point(p) => h.points.push(p),
                        ConstraintRef::Entity(e) => h.entities.push(e),
                    }
                }
            }
        }
        if let Some(e) = entity_hover {
            h.glyphs
                .extend(self.sketch.constraints_on(ConstraintRef::Entity(e)));
        }
        h
    }

    /// Screen-Space-Position + Symbol jedes anzuzeigenden Constraint-Glyphs.
    /// Glyphen desselben Ankers werden gestapelt (kein Überlappen).
    /// Distance/Radius-Constraints bekommen keinen Glyph — sie erscheinen
    /// bereits als Bemaßungs-Annotation.
    fn constraint_glyphs(&self, map: &PlaneMap) -> Vec<Glyph> {
        let mut used: HashMap<ConstraintRef, u32> = HashMap::new();
        let mut out = Vec::new();
        for (id, _) in self.sketch.constraints() {
            let Some(info) = self.sketch.constraint_info(id) else {
                continue;
            };
            // Bemaßungs-getriebene Constraints zeigt schon die Annotation.
            if info.dimension.is_some() {
                continue;
            }
            let Some(symbol) = constraint_glyph(info.kind) else {
                continue;
            };
            let Some(&anchor) = info.refs.first() else {
                continue;
            };
            let Some(pos) = self.ref_position(anchor) else {
                continue;
            };
            let slot = used.entry(anchor).or_insert(0);
            let base = map.plane_to_screen(pos) + GLYPH_OFFSET_PX;
            let center = base + egui::vec2((GLYPH_SIZE_PX + GLYPH_GAP_PX) * *slot as f32, 0.0);
            *slot += 1;
            out.push(Glyph { id, symbol, center });
        }
        out
    }

    /// Ankerposition eines referenzierten Elements in Ebenen-Koordinaten
    /// (Punkt bzw. Linienmitte/Kreismittelpunkt).
    fn ref_position(&self, r: ConstraintRef) -> Option<[f64; 2]> {
        match r {
            ConstraintRef::Point(p) => self
                .sketch
                .points()
                .find(|(id, _)| *id == p)
                .map(|(_, pos)| pos),
            ConstraintRef::Entity(e) => match self.sketch.entity(e)? {
                SketchEntity::Line { p1, p2 } => {
                    let a = self.sketch.point_pos(*p1);
                    let b = self.sketch.point_pos(*p2);
                    Some([(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5])
                }
                SketchEntity::Circle { center, .. } => Some(self.sketch.point_pos(*center)),
            },
        }
    }

    /// Constraint-Glyph unter dem Bildschirm-Cursor (falls sichtbar).
    fn glyph_at_screen(&self, map: &PlaneMap, cursor: Pos2) -> Option<ConstraintId> {
        let mut best: Option<(ConstraintId, f32)> = None;
        for g in self.constraint_glyphs(map) {
            let rect = Rect::from_center_size(g.center, Vec2::splat(GLYPH_SIZE_PX));
            if rect.contains(cursor) {
                let d = (g.center - cursor).length();
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((g.id, d));
                }
            }
        }
        best.map(|(id, _)| id)
    }

    /// Zeichnet die Constraint-Glyphen als kleine Chips (Screen-Space).
    fn paint_glyphs(&self, painter: &egui::Painter, map: &PlaneMap, highlight: &HoverHighlight) {
        if !self.show_constraint_glyphs {
            return;
        }
        for g in self.constraint_glyphs(map) {
            let selected = self.selection.contains(&Selected::Constraint(g.id));
            let hot = highlight.glyphs.contains(&g.id);
            let color = if selected {
                COLOR_SELECTED
            } else if hot {
                COLOR_GLYPH_HOVER
            } else {
                COLOR_GLYPH
            };
            let rect = Rect::from_center_size(g.center, Vec2::splat(GLYPH_SIZE_PX));
            painter.rect_filled(rect, 3.0, COLOR_DIM_LABEL_BG);
            if selected || hot {
                painter.rect_stroke(rect, 3.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
            }
            painter.text(
                g.center,
                egui::Align2::CENTER_CENTER,
                g.symbol,
                FontId::proportional(GLYPH_SIZE_PX * 0.72),
                color,
            );
        }
    }

    /// Beschreibung des (einzeln) selektierten Constraints für die
    /// Statuszeile: Art + referenzierte Geometrie. `None`, wenn kein
    /// Constraint selektiert ist.
    pub fn selected_constraint_text(&self) -> Option<String> {
        self.selection.iter().find_map(|s| match s {
            Selected::Constraint(id) => self.describe_constraint(*id),
            _ => None,
        })
    }

    fn describe_constraint(&self, id: ConstraintId) -> Option<String> {
        let info = self.sketch.constraint_info(id)?;
        let lines = info
            .refs
            .iter()
            .filter(|r| matches!(r, ConstraintRef::Entity(_)))
            .count();
        let points = info.refs.len() - lines;
        let mut parts = Vec::new();
        if lines > 0 {
            parts.push(format!("{lines} Linie{}", if lines == 1 { "" } else { "n" }));
        }
        if points > 0 {
            parts.push(format!(
                "{points} Punkt{}",
                if points == 1 { "" } else { "e" }
            ));
        }
        Some(format!(
            "{} · {}",
            constraint_kind_label(info.kind),
            parts.join(", ")
        ))
    }
}

/// Highlight-Zustand fürs Cross-Highlighting (leer = nichts hervorheben).
#[derive(Default)]
struct HoverHighlight {
    /// Hervorzuhebende Entities (aus einem gehoverten Glyph).
    entities: Vec<EntityId>,
    /// Hervorzuhebende Punkte (aus einem gehoverten Glyph).
    points: Vec<PointId>,
    /// Hervorzuhebende Glyphen (aus gehovertem Entity oder Glyph).
    glyphs: Vec<ConstraintId>,
}

/// Ein platzierter Constraint-Glyph im Screen-Space.
struct Glyph {
    id: ConstraintId,
    symbol: &'static str,
    center: Pos2,
}

/// Symbol eines Constraint-Glyphs; `None` für werthafte Constraints
/// (Distance/Radius), die bereits als Bemaßung erscheinen.
fn constraint_glyph(kind: ConstraintKind) -> Option<&'static str> {
    Some(match kind {
        ConstraintKind::Coincident => "●",
        ConstraintKind::Horizontal => "H",
        ConstraintKind::Vertical => "V",
        ConstraintKind::Parallel => "∥",
        ConstraintKind::Perpendicular => "⊥",
        ConstraintKind::Equal => "=",
        ConstraintKind::Distance | ConstraintKind::Radius => return None,
    })
}

/// Deutscher Anzeigename einer Constraint-Art.
fn constraint_kind_label(kind: ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::Coincident => "Koinzident",
        ConstraintKind::Horizontal => "Horizontal",
        ConstraintKind::Vertical => "Vertikal",
        ConstraintKind::Parallel => "Parallel",
        ConstraintKind::Perpendicular => "Senkrecht",
        ConstraintKind::Distance => "Abstand",
        ConstraintKind::Radius => "Radius",
        ConstraintKind::Equal => "Gleich lang",
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

/// Zeichnet eine (bestehende oder Vorschau-) Bemaßungsgeometrie.
/// `fill_label` blendet den Label-Hintergrund ein (bei der Werteingabe
/// deckt das Eingabefeld das Label ab, dann `false`).
fn draw_dim_geom(painter: &egui::Painter, geom: &DimGeom, color: Color32, fill_label: bool) {
    let stroke = Stroke::new(1.4, color);
    for seg in &geom.lines {
        painter.line_segment(*seg, stroke);
    }
    for &(tip, dir) in &geom.arrows {
        draw_arrowhead(painter, tip, dir, color);
    }
    let galley = painter.layout_no_wrap(geom.text.clone(), FontId::proportional(DIM_FONT_PX), color);
    if fill_label {
        let bg = Rect::from_center_size(geom.label, galley.size() + egui::vec2(6.0, 2.0));
        painter.rect_filled(bg, 3.0, COLOR_DIM_LABEL_BG);
    }
    painter.galley(geom.label - galley.size() * 0.5, galley, color);
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
    fn toggle_switches_only_circle_targets() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let c = session.sketch.add_point([0.0, 0.0]);
        let circle = session.sketch.add_circle(c, 1.0);
        let p = session.sketch.add_point([1.0, 0.0]);
        assert_eq!(
            toggle_circle_target(DimensionTarget::Diameter(circle)),
            DimensionTarget::Radius(circle)
        );
        assert_eq!(
            toggle_circle_target(DimensionTarget::Radius(circle)),
            DimensionTarget::Diameter(circle)
        );
        // Lineare Ziele bleiben unverändert.
        let linear = DimensionTarget::Linear(c, p);
        assert_eq!(toggle_circle_target(linear), linear);
    }

    #[test]
    fn measured_values_match_geometry() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let p1 = session.sketch.add_point([0.0, 0.0]);
        let p2 = session.sketch.add_point([3.0, 4.0]);
        let c = session.sketch.add_point([0.0, 0.0]);
        let circle = session.sketch.add_circle(c, 2.0);
        assert_eq!(session.measured_value(DimensionTarget::Linear(p1, p2)), 5.0);
        assert_eq!(session.measured_value(DimensionTarget::Radius(circle)), 2.0);
        assert_eq!(
            session.measured_value(DimensionTarget::Diameter(circle)),
            4.0
        );
    }

    #[test]
    fn dim_pick_first_click_resolves_geometry() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let p1 = session.sketch.add_point([0.0, 0.0]);
        let p2 = session.sketch.add_point([10.0, 0.0]);
        let _line = session.sketch.add_line(p1, p2);
        let cc = session.sketch.add_point([0.0, 5.0]);
        let circle = session.sketch.add_circle(cc, 2.0);
        let tol = 0.5;

        // Klick auf die Linienmitte → Längenbemaßung.
        assert_eq!(
            session.dim_pick(None, [5.0, 0.0], tol),
            DimStage::Placing {
                target: DimensionTarget::Linear(p1, p2)
            }
        );
        // Klick auf den Kreisring → Durchmesser (Default).
        assert_eq!(
            session.dim_pick(None, [0.0, 7.0], tol),
            DimStage::Placing {
                target: DimensionTarget::Diameter(circle)
            }
        );
        // Klick auf einen Endpunkt → Punkt-zu-Punkt beginnt.
        assert_eq!(
            session.dim_pick(None, [0.0, 0.0], tol),
            DimStage::Selecting {
                first_point: Some(p1)
            }
        );
    }

    #[test]
    fn dim_pick_second_point_forms_linear_or_resets() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let p1 = session.sketch.add_point([0.0, 0.0]);
        let p2 = session.sketch.add_point([10.0, 0.0]);
        let tol = 0.5;

        // Zweiter Punkt gewählt → lineare Bemaßung.
        assert_eq!(
            session.dim_pick(Some(p1), [10.0, 0.0], tol),
            DimStage::Placing {
                target: DimensionTarget::Linear(p1, p2)
            }
        );
        // Klick ins Leere → Auswahl sauber zurückgesetzt (kein Crash).
        assert_eq!(
            session.dim_pick(Some(p1), [5.0, 5.0], tol),
            DimStage::Selecting { first_point: None }
        );
    }

    #[test]
    fn dimension_button_yields_empty_selecting_stage() {
        assert_eq!(
            SketchTool::dimension().label(),
            SketchTool::Dimension(DimStage::Selecting { first_point: None }).label()
        );
        assert!(matches!(
            SketchTool::dimension(),
            SketchTool::Dimension(DimStage::Selecting { first_point: None })
        ));
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

    /// Akzeptanz 3: Doppelklick-Editieren bereitet für alle drei
    /// Bemaßungsarten das korrekte Eingabefeld vor (bestehende ID + Ziel).
    #[test]
    fn double_click_edit_prepares_input_for_all_kinds() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let p1 = session.sketch.add_point([0.0, 0.0]);
        let p2 = session.sketch.add_point([10.0, 0.0]);
        session.sketch.add_line(p1, p2);
        let c1 = session.sketch.add_point([0.0, 20.0]);
        let circle1 = session.sketch.add_circle(c1, 3.0);
        let c2 = session.sketch.add_point([0.0, 40.0]);
        let circle2 = session.sketch.add_circle(c2, 5.0);

        let lin = session
            .sketch
            .add_dimension(DimensionTarget::Linear(p1, p2), 10.0, [0.0, -2.0])
            .unwrap();
        let rad = session
            .sketch
            .add_dimension(DimensionTarget::Radius(circle1), 3.0, [2.0, 2.0])
            .unwrap();
        let dia = session
            .sketch
            .add_dimension(DimensionTarget::Diameter(circle2), 10.0, [3.0, 3.0])
            .unwrap();

        for (id, expected, expected_text) in [
            (lin, DimensionTarget::Linear(p1, p2), "10.0000"),
            (rad, DimensionTarget::Radius(circle1), "3.0000"),
            (dia, DimensionTarget::Diameter(circle2), "10.0000"),
        ] {
            match session.begin_edit_dimension(id).expect("edit stage") {
                DimStage::Editing {
                    target,
                    existing,
                    focus,
                    text,
                    ..
                } => {
                    assert_eq!(target, expected);
                    assert_eq!(existing, Some(id));
                    assert!(focus);
                    assert_eq!(text, expected_text);
                }
                other => panic!("erwartet Editing, war {other:?}"),
            }
        }

        // Der zugrunde liegende Edit ändert den Wert (least motion).
        session.sketch.set_dimension_value(dia, 12.0).expect("edit");
        let tol = 1e-4;
        assert!((session.sketch.circle_radius(circle2).unwrap() - 6.0).abs() < tol);
    }

    /// Akzeptanz 1: geometrische Constraints bekommen je einen Glyph;
    /// mehrere am selben Anker stapeln sich überlappungsfrei.
    #[test]
    fn geometric_constraints_get_stacking_glyphs() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let a = session.sketch.add_point([0.0, 0.0]);
        let b = session.sketch.add_point([10.0, 0.0]);
        let c = session.sketch.add_point([0.0, 5.0]);
        let d = session.sketch.add_point([10.0, 5.0]);
        let l1 = session.sketch.add_line(a, b);
        let l2 = session.sketch.add_line(c, d);
        session.sketch.add_constraint(Constraint::Horizontal(l1)).unwrap();
        session.sketch.add_constraint(Constraint::Parallel(l1, l2)).unwrap();
        session.sketch.add_constraint(Constraint::Equal(l1, l2)).unwrap();
        session.sketch.add_constraint(Constraint::Coincident(a, c)).unwrap();

        let map = PlaneMap::new(SketchPlane::XY, &camera, viewport());
        let glyphs = session.constraint_glyphs(&map);
        // Vier Glyphen (H, ∥, =, ●).
        assert_eq!(glyphs.len(), 4);
        // H/∥/= ankern an l1 (gestapelt), ● an Punkt a — keine zwei decken
        // sich, der Stapel läuft überlappungsfrei nebeneinander.
        let centers: Vec<_> = glyphs.iter().map(|g| g.center).collect();
        for i in 0..centers.len() {
            for j in (i + 1)..centers.len() {
                assert!(
                    (centers[i] - centers[j]).length() >= GLYPH_SIZE_PX,
                    "Glyphen überlappen: {:?} vs {:?}",
                    centers[i],
                    centers[j]
                );
            }
        }
    }

    /// Ein von einer Bemaßung getriebener Distance-Constraint bekommt keinen
    /// Glyph (die Bemaßungs-Annotation zeigt die Beziehung schon).
    #[test]
    fn dimension_driven_constraint_shows_no_glyph() {
        let (session, camera, _dim) = linear_session();
        let map = PlaneMap::new(SketchPlane::XY, &camera, viewport());
        assert!(session.constraint_glyphs(&map).is_empty());
    }

    /// Akzeptanz 1: Cross-Highlighting funktioniert in beide Richtungen.
    #[test]
    fn glyph_hover_cross_highlights_in_both_directions() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let a = session.sketch.add_point([0.0, 0.0]);
        let b = session.sketch.add_point([10.0, 0.0]);
        let l1 = session.sketch.add_line(a, b);
        session.sketch.add_constraint(Constraint::Horizontal(l1)).unwrap();

        let map = PlaneMap::new(SketchPlane::XY, &camera, viewport());
        let glyphs = session.constraint_glyphs(&map);
        assert_eq!(glyphs.len(), 1);
        let cid = glyphs[0].id;

        // Glyph unter seinem eigenen Zentrum getroffen.
        assert_eq!(session.glyph_at_screen(&map, glyphs[0].center), Some(cid));
        // Glyph-Hover → referenzierte Linie hervorgehoben.
        let h = session.hover_highlight(Some(cid), None);
        assert!(h.entities.contains(&l1));
        assert!(h.glyphs.contains(&cid));
        // Entity-Hover → sein Glyph hervorgehoben (Gegenrichtung).
        let h2 = session.hover_highlight(None, Some(l1));
        assert!(h2.glyphs.contains(&cid));
    }

    /// Akzeptanz 2: einen Constraint löschen entfernt seinen Glyph.
    #[test]
    fn deleting_constraint_removes_its_glyph() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let a = session.sketch.add_point([0.0, 0.0]);
        let b = session.sketch.add_point([10.0, 0.0]);
        let c = session.sketch.add_point([10.0, 0.0]);
        let d = session.sketch.add_point([10.0, 8.0]);
        let l1 = session.sketch.add_line(a, b);
        let l2 = session.sketch.add_line(c, d);
        let perp = session
            .sketch
            .add_constraint(Constraint::Perpendicular(l1, l2))
            .unwrap();

        let map = PlaneMap::new(SketchPlane::XY, &camera, viewport());
        assert_eq!(session.constraint_glyphs(&map).len(), 1);
        session.sketch.delete_constraint(perp);
        assert!(session.constraint_glyphs(&map).is_empty());
    }

    #[test]
    fn selected_constraint_text_describes_kind_and_refs() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let a = session.sketch.add_point([0.0, 0.0]);
        let b = session.sketch.add_point([10.0, 0.0]);
        let l1 = session.sketch.add_line(a, b);
        let hor = session
            .sketch
            .add_constraint(Constraint::Horizontal(l1))
            .unwrap();
        assert!(session.selected_constraint_text().is_none());
        session.selection = vec![Selected::Constraint(hor)];
        let text = session.selected_constraint_text().unwrap();
        assert!(text.contains("Horizontal"), "war {text}");
        assert!(text.contains("1 Linie"), "war {text}");
    }

    #[test]
    fn rejected_dimension_records_structured_error() {
        let mut camera = OrbitCamera::default();
        let mut session = SketchSession::start(SketchPlane::XY, &mut camera);
        let p1 = session.sketch.add_point([0.0, 0.0]);
        let p2 = session.sketch.add_point([10.0, 0.0]);
        session.sketch.add_line(p1, p2);

        session.commit_new_dimension(DimensionTarget::Linear(p1, p2), 10.0, [0.0, -2.0]);
        assert!(session.last_dim_error.is_none());
        assert_eq!(session.sketch.dimension_count(), 1);

        // Zweites, redundantes Maß auf dieselbe Strecke → Overconstraining.
        session.commit_new_dimension(DimensionTarget::Linear(p1, p2), 10.0, [0.0, -3.0]);
        assert!(matches!(
            session.last_dim_error,
            Some(DimensionError::Overconstraining { .. })
        ));
        assert_eq!(session.sketch.dimension_count(), 1);
    }
}
