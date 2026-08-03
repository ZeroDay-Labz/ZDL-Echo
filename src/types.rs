//! Cross-thread messages between the UI loop, the audio engine, and the
//! PipeWire routing engine.

/// One application detected on the PipeWire graph that ZDL-Echo could route
/// tones into (`can_tx`, its own mic/capture stream) or decode from
/// (`can_rx`, its own speaker/playback stream). `linked_tx`/`linked_rx`
/// reflect whether ZDL-Echo currently has a link into/out of it.
#[derive(Debug, Clone, PartialEq)]
pub struct SoftwareApp {
    pub label: String,
    pub can_tx: bool,
    pub can_rx: bool,
    pub linked_tx: bool,
    pub linked_rx: bool,
}

/// UI -> PipeWire-routing-thread commands, addressed by the app's display
/// `label` (re-resolved to live node ids at send time since ids churn as
/// apps restart their streams).
#[derive(Debug, Clone)]
pub enum PwCommand {
    LinkTx(String),
    UnlinkTx(String),
    LinkRx(String),
    UnlinkRx(String),
}

/// Handle for sending commands to the software-routing engine. On Linux
/// that engine is real (see `pw_route.rs`) and this is pipewire's own
/// channel sender; elsewhere it's an inert stub so `app.rs`/`main.rs` don't
/// need to branch on platform themselves.
#[cfg(target_os = "linux")]
pub type PwSender = pipewire::channel::Sender<PwCommand>;

#[cfg(not(target_os = "linux"))]
#[derive(Clone)]
pub struct PwSender;

#[cfg(not(target_os = "linux"))]
impl PwSender {
    pub fn send(&self, msg: PwCommand) -> Result<(), PwCommand> {
        let _ = msg;
        Ok(())
    }
}

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
    /// Toggle 2600 Hz single-frequency RX detection at runtime.
    SetDetectSf(bool),
    AudioStatus(String),
    AudioError(String),
    /// Negotiated stream latency (ms), reported after a stream is (re)built so
    /// the UI can size its self-echo RX mute window to the real audio path.
    StreamLatency { output: bool, ms: f32 },

    /// Latest snapshot of PipeWire applications available for software
    /// routing (Linux only; empty elsewhere).
    SoftwareApps(Vec<SoftwareApp>),
}