#![windows_subsystem = "windows"]
#![allow(deprecated)]

mod app;
mod audio;
mod decoder;
mod generator;
mod types;

use crossbeam_channel::unbounded;
use types::AppMessage;

fn main() -> eframe::Result<()> {
    // UI -> audio engine (tone commands)
    let (tx_cmd, rx_cmd) = unbounded::<AppMessage>();
    // audio engine -> UI (errors / status)
    let (tx_ui, rx_ui) = unbounded::<AppMessage>();

    std::thread::spawn(move || {
        audio::run_audio_engine(tx_ui, rx_cmd);
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 720.0])
            .with_min_inner_size([540.0, 620.0])
            .with_title("ZDL-ECHO"),
        ..Default::default()
    };

    eframe::run_native(
        "ZDL-ECHO",
        options,
        Box::new(|_cc| Ok(Box::new(app::DtmfApp::new(tx_cmd, rx_ui)))),
    )
}