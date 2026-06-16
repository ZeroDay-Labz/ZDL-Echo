//! Cross-thread messages between the UI loop and the audio engine.

#[derive(Debug, Clone)]
pub enum AppMessage {
    // RX: incoming signals captured from the audio stream
    DetectedTone(char),
    /// Live input peak level (0.0..~1.0) for the RX meter.
    RxLevel(f32),
    /// Most-recent captured samples (decimated) for the oscilloscope.
    RxWaveform(Vec<f32>),

    // TX: outgoing tone commands
    PlayTone { f1: f32, f2: f32, ms: u32 },
    StopAllTones,

    // System & hardware commands
    SetInputDevice(String),
    SetOutputDevice(String),
    AudioStatus(String),
    AudioError(String),
}