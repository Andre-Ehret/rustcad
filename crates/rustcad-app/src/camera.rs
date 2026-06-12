use glam::{Mat4, Vec3};

/// Orbit-Kamera mit Z-Achse als "oben" (CAD-Konvention).
#[derive(Clone)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    /// Drehung um die Z-Achse, im Bogenmaß.
    pub yaw: f32,
    /// Winkel über der XY-Ebene, im Bogenmaß.
    pub pitch: f32,
    pub fov_y: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::new(0.0, 0.0, 0.8),
            distance: 9.0,
            yaw: (-60.0f32).to_radians(),
            pitch: 28.0f32.to_radians(),
            fov_y: 45.0f32.to_radians(),
        }
    }
}

impl OrbitCamera {
    pub fn eye(&self) -> Vec3 {
        let dir = Vec3::new(
            self.pitch.cos() * self.yaw.cos(),
            self.pitch.cos() * self.yaw.sin(),
            self.pitch.sin(),
        );
        self.target + self.distance * dir
    }

    pub fn forward(&self) -> Vec3 {
        (self.target - self.eye()).normalize()
    }

    /// Projektionsmatrix. Clip-Ebenen relativ zur Distanz halten die
    /// Depth-Präzision stabil, egal wie weit gezoomt wird.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        let near = (self.distance * 0.01).max(1e-3);
        let far = self.distance * 100.0;
        Mat4::perspective_rh(self.fov_y, aspect.max(0.01), near, far)
    }

    /// Weltlänge pro Bildschirm-Pixel auf der Target-Ebene.
    pub fn world_per_pixel(&self, viewport_height_px: f32) -> f32 {
        2.0 * self.distance * (self.fov_y * 0.5).tan() / viewport_height_px.max(1.0)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * Mat4::look_at_rh(self.eye(), self.target, Vec3::Z)
    }

    /// Rotation per Maus-Drag; `delta` in Pixeln (egui: +y nach unten).
    pub fn orbit(&mut self, delta: egui::Vec2) {
        const SENSITIVITY: f32 = 0.008;
        self.yaw -= delta.x * SENSITIVITY;
        self.pitch =
            (self.pitch - delta.y * SENSITIVITY).clamp(-89.0f32.to_radians(), 89.0f32.to_radians());
    }

    /// Verschiebt das Orbit-Zentrum in der Bildebene; `delta` in Pixeln.
    /// `right`/`up` sind die Bildschirmachsen in Weltkoordinaten — so bleibt
    /// die Geometrie unter dem Cursor "kleben".
    pub fn pan_along(&mut self, delta: egui::Vec2, viewport_height_px: f32, right: Vec3, up: Vec3) {
        let world_per_pixel = self.world_per_pixel(viewport_height_px);
        self.target += (-right * delta.x + up * delta.y) * world_per_pixel;
    }

    /// Pan in der aktuellen Orbit-Ansicht.
    pub fn pan(&mut self, delta: egui::Vec2, viewport_height_px: f32) {
        let forward = self.forward();
        let right = forward.cross(Vec3::Z).normalize();
        let up = right.cross(forward);
        self.pan_along(delta, viewport_height_px, right, up);
    }

    /// Zoom per Scroll; positives `scroll_y` zoomt hinein.
    pub fn zoom(&mut self, scroll_y: f32) {
        self.distance = (self.distance * (-scroll_y * 0.002).exp()).clamp(0.05, 1e4);
    }

    /// Richtet die Kamera so aus, dass die Bounding-Box komplett sichtbar ist.
    pub fn fit(&mut self, min: [f32; 3], max: [f32; 3]) {
        let min = Vec3::from(min);
        let max = Vec3::from(max);
        self.target = (min + max) * 0.5;
        let radius = ((max - min).length() * 0.5).max(0.1);
        self.distance = radius / (self.fov_y * 0.5).sin() * 1.2;
    }
}
