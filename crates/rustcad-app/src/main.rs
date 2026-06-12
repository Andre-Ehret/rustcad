//! RustCAD — parametrischer 3D-CAD-Prototyp (siehe TECH_SPEC.md).

mod app;
mod camera;
mod renderer;
mod sketch_mode;

fn main() -> eframe::Result {
    env_logger::init();

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        // 32 Bit → Depth32Float; muss zu DEPTH_FORMAT im Renderer passen
        depth_buffer: 32,
        viewport: egui::ViewportBuilder::default()
            .with_title("RustCAD")
            .with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "RustCAD",
        options,
        Box::new(|cc| {
            app::RustcadApp::new(cc)
                .map(|a| Box::new(a) as Box<dyn eframe::App>)
                .map_err(Into::into)
        }),
    )
}
