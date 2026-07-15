use anyhow::Context;
use rustcad_core::{
    rebuild, Document, Feature, FeatureId, FeatureStatus, RebuildState, RevolveAxis, SketchPlane,
};
use rustcad_geom::TriMesh;

use crate::camera::OrbitCamera;
use crate::renderer::{encode_pick, PickId, SceneCallback, SceneRenderer, Uniforms};
use crate::sketch_mode::{self, SketchSession, SketchTool};

/// Interaktions-Zustandsmaschine (TECH_SPEC §7.4). Jeder Tool-Wechsel
/// läuft über dieses Enum.
enum AppMode {
    Idle,
    SketchEdit(Box<SketchSession>),
    FeatureDialog(PendingFeature),
}

/// Parameter eines Features, das gerade im Dialog konfiguriert wird.
enum PendingFeature {
    Extrude {
        sketch: FeatureId,
        profile: usize,
        distance: f64,
    },
    Revolve {
        sketch: FeatureId,
        profile: usize,
        angle_deg: f64,
        axis: RevolveAxis,
    },
}

enum ToolbarAction {
    None,
    Open,
    Save,
    ExportStl,
    EnterSketch(SketchPlane),
    SetTool(SketchTool),
    AddAction(sketch_mode::SketchAction),
    FinishSketch,
    OpenExtrude,
    OpenRevolve,
}

pub struct RustcadApp {
    mode: AppMode,
    camera: OrbitCamera,
    render_state: egui_wgpu::RenderState,
    document: Document,
    rebuild_state: RebuildState,
    /// Erzeugendes Feature je GPU-Body (parallel zur Body-Reihenfolge).
    body_features: Vec<FeatureId>,
    selected_feature: Option<FeatureId>,
    selected_face: Option<PickId>,
    /// Snapshot-basiertes Undo: Stack von Document-Klonen (TECH_SPEC M6).
    undo_stack: Vec<Document>,
    info: Option<String>,
}

const MAX_UNDO: usize = 32;

impl RustcadApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        let render_state = cc
            .wgpu_render_state
            .clone()
            .context("eframe wurde ohne wgpu-Renderer gestartet")?;

        let renderer = SceneRenderer::new(&render_state);
        render_state
            .renderer
            .write()
            .callback_resources
            .insert(renderer);

        Ok(Self {
            mode: AppMode::Idle,
            camera: OrbitCamera::default(),
            render_state,
            document: Document::new(),
            rebuild_state: RebuildState::default(),
            body_features: Vec::new(),
            selected_feature: None,
            selected_face: None,
            undo_stack: Vec::new(),
            info: None,
        })
    }

    /// Rebuild ab Historien-Index `from` + GPU-Sync der Bodies.
    fn do_rebuild(&mut self, from: usize) {
        rebuild(&self.document, from, &mut self.rebuild_state);
        let bodies = self.rebuild_state.bodies();
        self.body_features = bodies.iter().map(|(id, _)| *id).collect();
        let meshes: Vec<&TriMesh> = bodies.iter().map(|&(_, m)| m).collect();
        let mut renderer = self.render_state.renderer.write();
        if let Some(scene) = renderer.callback_resources.get_mut::<SceneRenderer>() {
            scene.set_bodies(&self.render_state.device, &meshes);
        }
        drop(renderer);
        // Body-Indizes können sich verschoben haben
        self.selected_face = None;
    }

    fn push_undo(&mut self, snapshot: Document) {
        self.undo_stack.push(snapshot);
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
    }

    fn undo(&mut self) {
        if let Some(document) = self.undo_stack.pop() {
            self.document = document;
            if self
                .selected_feature
                .is_some_and(|id| self.document.feature(id).is_none())
            {
                self.selected_feature = None;
            }
            self.do_rebuild(0);
        }
    }

    fn open_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("RustCAD", &["rcad"])
            .pick_file()
        else {
            return;
        };
        match rustcad_core::load_document(&path) {
            Ok(document) => {
                self.document = document;
                self.undo_stack.clear();
                self.selected_feature = None;
                self.do_rebuild(0);
                let (min, max) = self.scene_bounds();
                self.camera.fit(min, max);
                self.info = Some(format!("Geladen: {}", path.display()));
            }
            Err(error) => self.info = Some(format!("Laden fehlgeschlagen: {error}")),
        }
    }

    fn save_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("RustCAD", &["rcad"])
            .set_file_name("modell.rcad")
            .save_file()
        else {
            return;
        };
        match rustcad_core::save_document(&self.document, &path) {
            Ok(()) => self.info = Some(format!("Gespeichert: {}", path.display())),
            Err(error) => self.info = Some(format!("Speichern fehlgeschlagen: {error}")),
        }
    }

    fn export_stl_dialog(&mut self) {
        let mut merged = TriMesh::default();
        for (_, mesh) in self.rebuild_state.bodies() {
            merged.merge(mesh);
        }
        if merged.indices.is_empty() {
            self.info = Some("Keine Bodies zum Exportieren".into());
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("STL", &["stl"])
            .set_file_name("modell.stl")
            .save_file()
        else {
            return;
        };
        match rustcad_geom::export_stl(&merged, &path) {
            Ok(()) => self.info = Some(format!("STL exportiert: {}", path.display())),
            Err(error) => self.info = Some(format!("Export fehlgeschlagen: {error}")),
        }
    }

    fn scene_bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut bounds: Option<([f32; 3], [f32; 3])> = None;
        for (_, mesh) in self.rebuild_state.bodies() {
            if let Some((min, max)) = mesh.bounding_box() {
                bounds = Some(match bounds {
                    None => (min, max),
                    Some((bmin, bmax)) => (
                        std::array::from_fn(|i| bmin[i].min(min[i])),
                        std::array::from_fn(|i| bmax[i].max(max[i])),
                    ),
                });
            }
        }
        bounds.unwrap_or(([-5.0, -5.0, -1.0], [5.0, 5.0, 4.0]))
    }

    fn feature_label(&self, index: usize, feature: &Feature) -> String {
        match feature {
            Feature::Sketch(s) => format!("{} · Skizze ({})", index + 1, s.plane.label()),
            Feature::Extrude(e) => format!("{} · Extrude (T={:.2})", index + 1, e.distance),
            Feature::Revolve(r) => {
                format!("{} · Revolve ({:.0}°)", index + 1, r.angle.to_degrees())
            }
        }
    }

    fn open_sketch_editor(&mut self, id: FeatureId) {
        if let Some(Feature::Sketch(sf)) = self.document.feature(id) {
            let session =
                SketchSession::start_edit(sf.plane, sf.sketch.clone(), id, &mut self.camera);
            self.mode = AppMode::SketchEdit(Box::new(session));
            self.selected_face = None;
            self.info = None;
        }
    }

    /// Feature-Baum (linkes Panel): Selektion, Fehlerzustände,
    /// Doppelklick auf Skizze öffnet den Skizzen-Editor.
    fn feature_tree(&mut self, ui: &mut egui::Ui) {
        ui.strong("Feature-Baum");
        ui.separator();
        if self.document.is_empty() {
            ui.weak("Noch keine Features.\nSkizze über die Toolbar anlegen.");
            return;
        }

        let mut clicked: Option<FeatureId> = None;
        let mut edit: Option<FeatureId> = None;
        for (index, (id, feature)) in self.document.features().enumerate() {
            let failed = match self.rebuild_state.status_of(id) {
                Some(FeatureStatus::Failed(msg)) => Some(msg.clone()),
                _ => None,
            };
            let label = self.feature_label(index, feature);
            let text = if failed.is_some() {
                egui::RichText::new(format!("⚠ {label}"))
                    .color(egui::Color32::from_rgb(230, 80, 70))
            } else {
                egui::RichText::new(label)
            };
            let mut response = ui.selectable_label(self.selected_feature == Some(id), text);
            if let Some(message) = failed {
                response = response.on_hover_text(message);
            }
            if response.clicked() {
                clicked = Some(id);
            }
            if response.double_clicked() && matches!(feature, Feature::Sketch(_)) {
                edit = Some(id);
            }
        }

        if let Some(id) = clicked {
            self.selected_feature = Some(id);
        }
        if let Some(id) = edit {
            self.open_sketch_editor(id);
        }
    }

    /// Eigenschaften des selektierten Features (rechtes Panel):
    /// Parameter editieren löst den Rebuild ab diesem Feature aus.
    fn properties_panel(&mut self, ui: &mut egui::Ui) {
        ui.strong("Eigenschaften");
        ui.separator();
        let Some(id) = self.selected_feature else {
            ui.weak("Kein Feature ausgewählt.");
            return;
        };

        // Snapshot vor der Änderung; bei Drags wird genau einmal
        // (am Drag-Beginn) auf den Undo-Stack gelegt
        let before = self.document.clone();
        let mut changed = false;
        let mut drag_started = false;
        let mut dragging = false;
        let mut edit_sketch = false;
        let mut delete = false;

        match self.document.feature_mut(id) {
            Some(Feature::Sketch(sf)) => {
                ui.label(format!("Skizze auf {}", sf.plane.label()));
                ui.label(format!(
                    "{} Entities · {} Constraints",
                    sf.sketch.entity_count(),
                    sf.sketch.constraint_count()
                ));
                ui.label(format!(
                    "{} geschlossene Profile",
                    sf.sketch.find_profiles().len()
                ));
                if ui.button("✏ Bearbeiten").clicked() {
                    edit_sketch = true;
                }
            }
            Some(Feature::Extrude(e)) => {
                let response = ui.add(
                    egui::DragValue::new(&mut e.distance)
                        .speed(0.1)
                        .range(-1000.0..=1000.0)
                        .prefix("Tiefe: "),
                );
                changed |= response.changed();
                drag_started |= response.drag_started();
                dragging |= response.dragged();
                let mut profile = e.profile as i32 + 1;
                let response = ui.add(
                    egui::DragValue::new(&mut profile)
                        .range(1..=32)
                        .prefix("Profil: "),
                );
                if response.changed() {
                    e.profile = (profile - 1).max(0) as usize;
                    changed = true;
                }
                drag_started |= response.drag_started();
                dragging |= response.dragged();
            }
            Some(Feature::Revolve(r)) => {
                let mut degrees = r.angle.to_degrees();
                let response = ui.add(
                    egui::DragValue::new(&mut degrees)
                        .speed(1.0)
                        .range(1.0..=360.0)
                        .prefix("Winkel: ")
                        .suffix("°"),
                );
                if response.changed() {
                    r.angle = degrees.to_radians();
                    changed = true;
                }
                drag_started |= response.drag_started();
                dragging |= response.dragged();
                ui.horizontal(|ui| {
                    ui.label("Achse:");
                    changed |= ui
                        .selectable_value(&mut r.axis, RevolveAxis::V, "v")
                        .changed();
                    changed |= ui
                        .selectable_value(&mut r.axis, RevolveAxis::U, "u")
                        .changed();
                });
                let mut profile = r.profile as i32 + 1;
                let response = ui.add(
                    egui::DragValue::new(&mut profile)
                        .range(1..=32)
                        .prefix("Profil: "),
                );
                if response.changed() {
                    r.profile = (profile - 1).max(0) as usize;
                    changed = true;
                }
                drag_started |= response.drag_started();
                dragging |= response.dragged();
            }
            None => {
                ui.weak("Feature existiert nicht mehr.");
            }
        }

        ui.separator();
        if ui.button("🗑 Löschen").clicked() {
            delete = true;
        }

        if delete {
            self.push_undo(before);
            let from = self.document.remove(id);
            self.selected_feature = None;
            self.do_rebuild(from);
        } else if changed {
            if drag_started || !dragging {
                self.push_undo(before);
            }
            let from = self.document.index_of(id).unwrap_or(0);
            self.do_rebuild(from);
        } else if edit_sketch {
            self.open_sketch_editor(id);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let mut action = ToolbarAction::None;

        ui.horizontal(|ui| {
            ui.strong("RustCAD");
            ui.separator();

            match &self.mode {
                AppMode::Idle | AppMode::FeatureDialog(_) => {
                    if ui.button("📂 Öffnen").clicked() {
                        action = ToolbarAction::Open;
                    }
                    if ui.button("💾 Speichern").clicked() {
                        action = ToolbarAction::Save;
                    }
                    if ui
                        .add_enabled(
                            !self.body_features.is_empty(),
                            egui::Button::new("STL Export"),
                        )
                        .clicked()
                    {
                        action = ToolbarAction::ExportStl;
                    }
                    ui.separator();
                    ui.label("Skizze:");
                    for plane in SketchPlane::ALL {
                        if ui.button(plane.label()).clicked() {
                            action = ToolbarAction::EnterSketch(plane);
                        }
                    }
                    ui.separator();
                    let has_sketch = !self.document.sketch_features().is_empty();
                    if ui
                        .add_enabled(has_sketch, egui::Button::new("Extrude"))
                        .clicked()
                    {
                        action = ToolbarAction::OpenExtrude;
                    }
                    if ui
                        .add_enabled(has_sketch, egui::Button::new("Revolve"))
                        .clicked()
                    {
                        action = ToolbarAction::OpenRevolve;
                    }
                    ui.separator();
                    let mut status = format!(
                        "Features: {} · Bodies: {}",
                        self.document.len(),
                        self.body_features.len()
                    );
                    if let Some((body, face)) = self.selected_face {
                        let feature = self
                            .body_features
                            .get(body as usize)
                            .and_then(|&fid| self.document.index_of(fid))
                            .map_or_else(
                                || format!("Body {body}"),
                                |i| format!("Feature {}", i + 1),
                            );
                        status.push_str(&format!(" · {feature}, Face {face}"));
                    }
                    ui.label(status);
                    if let Some(info) = &self.info {
                        ui.colored_label(egui::Color32::from_rgb(240, 180, 70), info);
                    }
                }
                AppMode::SketchEdit(session) => {
                    let tools = [
                        SketchTool::Select,
                        SketchTool::Line { start: None },
                        SketchTool::Circle { center: None },
                        SketchTool::dimension(),
                    ];
                    for tool in tools {
                        let active =
                            std::mem::discriminant(&session.tool) == std::mem::discriminant(&tool);
                        if ui.selectable_label(active, tool.label()).clicked() {
                            action = ToolbarAction::SetTool(tool);
                        }
                    }
                    ui.separator();
                    for (label, sketch_action) in session.available_actions() {
                        if ui.button(label).clicked() {
                            action = ToolbarAction::AddAction(sketch_action);
                        }
                    }
                    ui.separator();
                    if ui.button("✔ Fertig").clicked() {
                        action = ToolbarAction::FinishSketch;
                    }
                    ui.separator();
                    let status = match session.last_solve {
                        Some(rustcad_sketch::SolveResult::DidNotConverge { .. }) => {
                            " · ⚠ nicht lösbar"
                        }
                        _ => "",
                    };
                    ui.label(format!(
                        "{} · {} Entities · {} Constraints · {} Maße · {} Freiheitsgrade{}",
                        session.plane.label(),
                        session.sketch.entity_count(),
                        session.sketch.constraint_count(),
                        session.sketch.dimension_count(),
                        session.sketch.dof(),
                        status,
                    ));
                }
            }
        });

        match action {
            ToolbarAction::None => {}
            ToolbarAction::Open => self.open_dialog(),
            ToolbarAction::Save => self.save_dialog(),
            ToolbarAction::ExportStl => self.export_stl_dialog(),
            ToolbarAction::EnterSketch(plane) => {
                self.info = None;
                self.selected_face = None;
                self.mode =
                    AppMode::SketchEdit(Box::new(SketchSession::start(plane, &mut self.camera)));
            }
            ToolbarAction::SetTool(tool) => {
                if let AppMode::SketchEdit(session) = &mut self.mode {
                    session.tool = tool;
                    session.selection.clear();
                }
            }
            ToolbarAction::AddAction(sketch_action) => {
                if let AppMode::SketchEdit(session) = &mut self.mode {
                    session.apply_action(sketch_action);
                }
            }
            ToolbarAction::FinishSketch => {
                if let AppMode::SketchEdit(session) =
                    std::mem::replace(&mut self.mode, AppMode::Idle)
                {
                    let (editing, plane, sketch) = session.finish(&mut self.camera);
                    let snapshot = self.document.clone();
                    match editing {
                        Some(id) => {
                            if let Some(Feature::Sketch(sf)) = self.document.feature_mut(id) {
                                sf.sketch = sketch;
                            }
                            self.push_undo(snapshot);
                            let from = self.document.index_of(id).unwrap_or(0);
                            self.do_rebuild(from);
                        }
                        None if sketch.entity_count() > 0 => {
                            let id = self.document.add_sketch(plane, sketch);
                            self.push_undo(snapshot);
                            self.do_rebuild(self.document.len().saturating_sub(1));
                            self.selected_feature = Some(id);
                        }
                        None => {}
                    }
                }
            }
            ToolbarAction::OpenExtrude => {
                if let Some(&(sketch, _)) = self.document.sketch_features().last() {
                    self.info = None;
                    self.mode = AppMode::FeatureDialog(PendingFeature::Extrude {
                        sketch,
                        profile: 0,
                        distance: 2.0,
                    });
                }
            }
            ToolbarAction::OpenRevolve => {
                if let Some(&(sketch, _)) = self.document.sketch_features().last() {
                    self.info = None;
                    self.mode = AppMode::FeatureDialog(PendingFeature::Revolve {
                        sketch,
                        profile: 0,
                        angle_deg: 360.0,
                        axis: RevolveAxis::V,
                    });
                }
            }
        }
    }

    /// Modaler Parameter-Dialog für das anstehende Feature.
    fn feature_dialog(&mut self, ctx: &egui::Context) {
        let AppMode::FeatureDialog(pending) = &mut self.mode else {
            return;
        };

        let title = match pending {
            PendingFeature::Extrude { .. } => "Extrude",
            PendingFeature::Revolve { .. } => "Revolve",
        };
        let mut confirmed = false;
        let mut cancelled = false;

        let sketches: Vec<(FeatureId, String, usize)> = self
            .document
            .features()
            .enumerate()
            .filter_map(|(i, (id, f))| match f {
                Feature::Sketch(s) => Some((
                    id,
                    format!("{} · Skizze ({})", i + 1, s.plane.label()),
                    s.sketch.find_profiles().len(),
                )),
                _ => None,
            })
            .collect();

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
            .show(ctx, |ui| {
                let (sketch, profile) = match pending {
                    PendingFeature::Extrude {
                        sketch, profile, ..
                    }
                    | PendingFeature::Revolve {
                        sketch, profile, ..
                    } => (sketch, profile),
                };

                let current_label = sketches
                    .iter()
                    .find(|(id, ..)| id == sketch)
                    .map_or("?", |(_, label, _)| label.as_str())
                    .to_owned();
                egui::ComboBox::from_label("Skizze")
                    .selected_text(current_label)
                    .show_ui(ui, |ui| {
                        for (id, label, _) in &sketches {
                            ui.selectable_value(sketch, *id, label);
                        }
                    });

                let profile_count = sketches
                    .iter()
                    .find(|(id, ..)| id == sketch)
                    .map_or(0, |&(_, _, n)| n);
                if profile_count > 1 {
                    let mut selected = *profile as i32 + 1;
                    ui.add(
                        egui::DragValue::new(&mut selected)
                            .range(1..=profile_count as i32)
                            .prefix("Profil: "),
                    );
                    *profile = (selected - 1).max(0) as usize;
                } else if profile_count == 0 {
                    ui.colored_label(
                        egui::Color32::from_rgb(230, 80, 70),
                        "Skizze enthält keine geschlossene Schleife",
                    );
                }

                match pending {
                    PendingFeature::Extrude { distance, .. } => {
                        ui.add(
                            egui::DragValue::new(distance)
                                .speed(0.1)
                                .range(-1000.0..=1000.0)
                                .prefix("Tiefe: "),
                        );
                    }
                    PendingFeature::Revolve {
                        angle_deg, axis, ..
                    } => {
                        ui.add(
                            egui::DragValue::new(angle_deg)
                                .speed(1.0)
                                .range(1.0..=360.0)
                                .prefix("Winkel: ")
                                .suffix("°"),
                        );
                        ui.horizontal(|ui| {
                            ui.label("Achse:");
                            ui.selectable_value(axis, RevolveAxis::V, "v (senkrecht)");
                            ui.selectable_value(axis, RevolveAxis::U, "u (waagerecht)");
                        });
                    }
                }

                ui.horizontal(|ui| {
                    if ui.button("✔ OK").clicked() {
                        confirmed = true;
                    }
                    if ui.button("Abbrechen").clicked() {
                        cancelled = true;
                    }
                });
            });

        if confirmed {
            let pending = match std::mem::replace(&mut self.mode, AppMode::Idle) {
                AppMode::FeatureDialog(p) => p,
                _ => unreachable!(),
            };
            let snapshot = self.document.clone();
            let id = match pending {
                PendingFeature::Extrude {
                    sketch,
                    profile,
                    distance,
                } => self.document.add_extrude(sketch, profile, distance),
                PendingFeature::Revolve {
                    sketch,
                    profile,
                    angle_deg,
                    axis,
                } => self
                    .document
                    .add_revolve(sketch, profile, axis, angle_deg.to_radians()),
            };
            self.push_undo(snapshot);
            self.do_rebuild(self.document.len().saturating_sub(1));
            self.selected_feature = Some(id);
            match self.rebuild_state.status_of(id) {
                Some(FeatureStatus::Failed(message)) => self.info = Some(message.clone()),
                _ => {
                    let (min, max) = self.scene_bounds();
                    self.camera.fit(min, max);
                }
            }
        } else if cancelled || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.mode = AppMode::Idle;
        }
    }

    fn viewport_ui(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());

        // Hintergrund "clearen" — der Callback zeichnet mitten im egui-Pass
        // und kann selbst kein Clear ausführen
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_rgb(24, 26, 31));

        let aspect = rect.aspect_ratio();
        let (view_proj, light_dir, hint) = match &mut self.mode {
            AppMode::Idle | AppMode::FeatureDialog(_) => {
                // Orbit: rechte Maustaste (solange keine 3D-Selektion existiert
                // auch linke); Pan: mittlere; Zoom: Scrollrad; F: fit view
                if response.dragged_by(egui::PointerButton::Secondary)
                    || response.dragged_by(egui::PointerButton::Primary)
                {
                    self.camera.orbit(response.drag_delta());
                }
                if response.dragged_by(egui::PointerButton::Middle) {
                    self.camera.pan(response.drag_delta(), rect.height());
                }
                if response.hovered() {
                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        self.camera.zoom(scroll);
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::F)) {
                        let (min, max) = self.scene_bounds();
                        self.camera.fit(min, max);
                    }
                }
                (
                    self.camera.view_proj(aspect),
                    self.camera.forward(),
                    "Klick: Fläche wählen  ·  Drag: Orbit  ·  Mitte: Pan  ·  Scroll: Zoom  ·  F: Fit  ·  ⌘Z: Undo",
                )
            }
            AppMode::SketchEdit(session) => (
                session.view_proj(&self.camera, aspect),
                session.forward(),
                "Klick: Zeichnen/Auswählen (Shift: mehrfach)  ·  Drag: Punkt ziehen  ·  Esc: Abbrechen  ·  Entf: Löschen",
            ),
        };

        // Face-Picking per ID-Buffer (nur Idle, nur bei Klick)
        if matches!(self.mode, AppMode::Idle) && response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                let ppp = ui.ctx().pixels_per_point();
                let size = [
                    (rect.width() * ppp).round() as u32,
                    (rect.height() * ppp).round() as u32,
                ];
                let pixel = [
                    ((pos.x - rect.min.x) * ppp).round() as u32,
                    ((pos.y - rect.min.y) * ppp).round() as u32,
                ];
                let renderer = self.render_state.renderer.read();
                self.selected_face =
                    renderer
                        .callback_resources
                        .get::<SceneRenderer>()
                        .and_then(|scene| {
                            scene.pick(
                                &self.render_state.device,
                                &self.render_state.queue,
                                view_proj.to_cols_array_2d(),
                                size,
                                pixel,
                            )
                        });
            }
        }
        if matches!(self.mode, AppMode::Idle) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selected_face = None;
        }

        let selected = self
            .selected_face
            .map_or(0, |(body, face)| encode_pick(body, face) + 1);

        // 3D-Szene (Bodies, Grid, Achsen)
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            SceneCallback {
                uniforms: Uniforms {
                    view_proj: view_proj.to_cols_array_2d(),
                    light_dir: [light_dir.x, light_dir.y, light_dir.z, 0.0],
                    selected: [selected, 0, 0, 0],
                },
            },
        ));

        // Dokument-Skizzen als Welt-Overlay (die gerade editierte nicht)
        let editing = match &self.mode {
            AppMode::SketchEdit(session) => session.editing,
            _ => None,
        };
        let painter = ui.painter_at(rect);
        for (id, sf) in self.document.sketch_features() {
            if Some(id) != editing {
                sketch_mode::paint_sketch(&painter, rect, view_proj, sf.plane, &sf.sketch);
            }
        }

        // Aktive Skizze: Eingaben + Overlay
        if let AppMode::SketchEdit(session) = &mut self.mode {
            session.handle_viewport(ui, rect, &response, &mut self.camera);
        }

        ui.painter().text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            hint,
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(140),
        );
    }
}

impl eframe::App for RustcadApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !matches!(self.mode, AppMode::SketchEdit(_))
            && ui
                .ctx()
                .input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z))
        {
            self.undo();
        }

        egui::Panel::top("toolbar").show_inside(ui, |ui| self.toolbar(ui));

        // Baum + Eigenschaften nur außerhalb des Skizzenmodus —
        // verhindert z. B. das Löschen der gerade editierten Skizze
        if !matches!(self.mode, AppMode::SketchEdit(_)) {
            egui::Panel::left("feature_tree")
                .resizable(true)
                .default_size(200.0)
                .show_inside(ui, |ui| self.feature_tree(ui));
            egui::Panel::right("properties")
                .resizable(true)
                .default_size(200.0)
                .show_inside(ui, |ui| self.properties_panel(ui));
        }

        self.feature_dialog(ui.ctx());

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.viewport_ui(ui));
    }
}
