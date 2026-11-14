mod app;
mod assets;
mod audio;
mod intro;
mod project;
mod runtime;
mod settings;
mod tts;

use eframe::egui;

fn load_icon(path: &str) -> Option<egui::IconData> {
    let img = image::open(path).ok()?.to_rgba8();
    let (width, height) = (img.width(), img.height());
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}

fn main() -> eframe::Result<()> {
    let settings = match settings::Settings::load("settings.json5") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let mut viewport = egui::ViewportBuilder::default()
            .with_inner_size([settings.window.width as f32, settings.window.height as f32])
            .with_title(settings.game.title.clone())
            .with_resizable(settings.window.resizable)
            .with_fullscreen(settings.window.fullscreen);
    if let Some(icon) = settings.window.icon.as_deref().and_then(load_icon) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        "Rouille3",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, settings)))),
    )
}
