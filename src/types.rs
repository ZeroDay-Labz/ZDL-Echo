//! Cross-thread messages between the UI loop and the audio engine.

#[derive(Debug, Clone)]
pub enum AppMessage {
    // RX: Incoming signals captured from the audio stream
    DetectedTone(char),

    // TX: Outgoing tone commands
    PlayTone { f1: f32, f2: f32, ms: u32 },
    StopAllTones,

    // System & Hardware Commands
    SetInputDevice(String),
    AudioStatus(String),
    AudioError(String),
}