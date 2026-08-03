#![windows_subsystem = "windows"]

mod app;
mod audio;
mod config;
mod decoder;
mod generator;
#[cfg(target_os = "linux")]
mod pw_route;
mod types;

use crossbeam_channel::unbounded;
use types::AppMessage;

fn main() -> eframe::Result<()> {
    // UI -> audio engine (tone commands)
    let (tx_cmd, rx_cmd) = unbounded::<AppMessage>();
    // audio engine -> UI (errors / status)
    let (tx_ui, rx_ui) = unbounded::<AppMessage>();

    let tx_ui_audio = tx_ui.clone();
    std::thread::spawn(move || {
        audio::run_audio_engine(tx_ui_audio, rx_cmd);
    });

    // Software (PipeWire application) routing — Linux only. UI -> pw thread
    // commands travel over pipewire's own channel type since pw objects
    // can't cross threads; pw thread -> UI reuses the crossbeam channel above.
    #[cfg(target_os = "linux")]
    let pw_cmd_tx: types::PwSender = {
        let (pw_cmd_tx, pw_cmd_rx) = pipewire::channel::channel::<types::PwCommand>();
        std::thread::spawn(move || {
            pw_route::run(tx_ui, pw_cmd_rx);
        });
        pw_cmd_tx
    };
    #[cfg(not(target_os = "linux"))]
    let pw_cmd_tx: types::PwSender = types::PwSender;

    // --- ICON LOADING ---
    // include_bytes embeds the .ico file directly into the compiled binary
    // The path is relative to the main.rs file.
    let icon_data = include_bytes!("../assets/zdl-echo.ico");
    let image = image::load_from_memory(icon_data)
        .expect("Failed to load icon from memory")
        .to_rgba8();
    let (icon_width, icon_height) = image.dimensions();
    let icon = egui::IconData {
        rgba: image.into_raw(),
        width: icon_width,
        height: icon_height,
    };
    // --------------------

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 720.0])
            .with_min_inner_size([540.0, 620.0])
            .with_title("ZDL-ECHO")
            .with_app_id("zdlecho") // <-- Wayland application ID binding
            .with_icon(icon),
        ..Default::default()
    };

    eframe::run_native(
        "ZDL-ECHO",
        options,
        Box::new(|_cc| Ok(Box::new(app::DtmfApp::new(tx_cmd, rx_ui, pw_cmd_tx)))),
    )
}

