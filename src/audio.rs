//! ZDL-Echo audio engine — TX transmitter & RX capture.
//!
//! Both the output (transmit) and input (capture) devices are selectable and
//! rebuilt on command. Every device/stream call is fallible: on failure we send
//! an AudioError to the UI and keep the engine alive instead of panicking, and
//! we convert i16/u16 hardware formats to/from f32 so non-float devices work.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use crate::types::AppMessage;
use crate::decoder::ToneDecoder;

const PI: f32 = std::f32::consts::PI;
/// Requested period size (frames) when the device exposes a configurable
/// buffer range — small enough for low latency, large enough to be safe on
/// most backends (PipeWire's ALSA shim happily grants this on Linux).
const PREFERRED_BUFFER_FRAMES: cpal::FrameCount = 256;

struct TxOscillator {
    phase_1: f32,
    phase_2: f32,
    freq_1: f32,
    freq_2: f32,
    sample_rate: f32,
    frames_remaining: usize,
}

impl TxOscillator {
    /// Next mono sample in [-1, 1]. 0.0 when nothing is playing.
    fn next_sample(&mut self) -> f32 {
        if self.frames_remaining == 0 {
            return 0.0;
        }
        self.frames_remaining -= 1;
        let s1 = (self.phase_1 * 2.0 * PI).sin();
        self.phase_1 = (self.phase_1 + self.freq_1 / self.sample_rate) % 1.0;
        if self.freq_2 > 0.0 {
            let s2 = (self.phase_2 * 2.0 * PI).sin();
            self.phase_2 = (self.phase_2 + self.freq_2 / self.sample_rate) % 1.0;
            (s1 + s2) * 0.25
        } else {
            s1 * 0.4
        }
    }
}

fn find_output_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.output_devices().ok()?.find(|d| d.to_string() == name)
}
fn find_input_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.input_devices().ok()?.find(|d| d.to_string() == name)
}

/// Build a stream config from `supported`, preferring a small fixed buffer
/// size over whatever the platform's default happens to negotiate. Falls
/// back to `BufferSize::Default` if the device won't report a usable range
/// (e.g. some CoreAudio/WASAPI paths) — never fails, just isn't as tight.
fn low_latency_config(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    is_output: bool,
) -> cpal::StreamConfig {
    let mut buffer_size = cpal::BufferSize::Default;
    let ranges: Vec<cpal::SupportedStreamConfigRange> = if is_output {
        device.supported_output_configs().map(|it| it.collect()).unwrap_or_default()
    } else {
        device.supported_input_configs().map(|it| it.collect()).unwrap_or_default()
    };
    for r in ranges {
        if r.channels() == supported.channels()
            && r.sample_format() == supported.sample_format()
            && r.min_sample_rate() <= supported.sample_rate()
            && supported.sample_rate() <= r.max_sample_rate()
            && let cpal::SupportedBufferSize::Range { min, max } = r.buffer_size() {
                buffer_size = cpal::BufferSize::Fixed(PREFERRED_BUFFER_FRAMES.clamp(*min, *max));
                break;
            }
    }
    cpal::StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size,
    }
}

/// Estimated one-way latency (ms) for `config` at `sample_rate`. Used only to
/// size the RX self-echo mute window, so a conservative fallback for
/// `BufferSize::Default` (whose real period isn't queryable) is fine.
fn estimate_latency_ms(config: &cpal::StreamConfig, sample_rate: f32) -> f32 {
    match config.buffer_size {
        cpal::BufferSize::Fixed(frames) => frames as f32 / sample_rate * 1000.0,
        cpal::BufferSize::Default => 20.0,
    }
}

/// Downmix interleaved frames of `T` to mono f32 via `conv`, reusing `out`'s
/// allocation instead of allocating fresh on every audio callback.
fn downmix_into<T: Copy>(data: &[T], channels: usize, conv: impl Fn(T) -> f32, out: &mut Vec<f32>) {
    let ch = channels.max(1);
    out.clear();
    out.extend(data.chunks(ch).map(|frame| frame.iter().map(|&s| conv(s)).sum::<f32>() / ch as f32));
}

/// `wave_buf` is reused across calls; only the final send to the UI thread
/// allocates (unavoidable — the channel takes ownership), and it's bounded
/// to `TARGET` samples.
fn emit(decoder: &mut ToneDecoder, mono: &[f32], wave_buf: &mut Vec<f32>, tx: &Sender<AppMessage>) {
    let peak = mono.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    let _ = tx.send(AppMessage::RxLevel(peak));

    // scope snapshot: decimate the buffer to ~256 points across its span
    const TARGET: usize = 256;
    wave_buf.clear();
    if mono.len() <= TARGET {
        wave_buf.extend_from_slice(mono);
    } else {
        let step = (mono.len() / TARGET).max(1);
        wave_buf.extend(mono.iter().step_by(step).take(TARGET).copied());
    }
    let _ = tx.send(AppMessage::RxWaveform(wave_buf.clone()));

    for (_, ch) in decoder.process_samples(mono) {
        let _ = tx.send(AppMessage::DetectedTone(ch));
    }
}

/// Build + start an output stream on `device`, in whatever format it supports.
fn build_output(
    device: &cpal::Device,
    tx_state: &Arc<Mutex<TxOscillator>>,
    tx_ui: &Sender<AppMessage>,
) -> Result<cpal::Stream, String> {
    let supported = device
        .default_output_config()
        .map_err(|e| format!("output config: {e}"))?;
    let fmt = supported.sample_format();
    let config = low_latency_config(device, &supported, true);
    let sr = config.sample_rate as f32;
    let channels = (config.channels as usize).max(1);
    let latency_ms = estimate_latency_ms(&config, sr);

    // keep the oscillator's clock matched to this device
    {
        let mut s = tx_state.lock().unwrap_or_else(|e| e.into_inner());
        s.sample_rate = sr;
    }
    let _ = tx_ui.send(AppMessage::StreamLatency { output: true, ms: latency_ms });

    let err_tx = tx_ui.clone();
    let on_err = move |e| {
        let _ = err_tx.send(AppMessage::AudioError(format!("TX stream: {e}")));
    };

    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let st = Arc::clone(tx_state);
            device.build_output_stream(
                config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut o = st.lock().unwrap_or_else(|e| e.into_inner());
                    for frame in data.chunks_mut(channels) {
                        let v = o.next_sample();
                        for ch in frame.iter_mut() {
                            *ch = v;
                        }
                    }
                },
                on_err,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let st = Arc::clone(tx_state);
            device.build_output_stream(
                config,
                move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let mut o = st.lock().unwrap_or_else(|e| e.into_inner());
                    for frame in data.chunks_mut(channels) {
                        let v = (o.next_sample().clamp(-1.0, 1.0) * 32767.0) as i16;
                        for ch in frame.iter_mut() {
                            *ch = v;
                        }
                    }
                },
                on_err,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let st = Arc::clone(tx_state);
            device.build_output_stream(
                config,
                move |data: &mut [u16], _: &cpal::OutputCallbackInfo| {
                    let mut o = st.lock().unwrap_or_else(|e| e.into_inner());
                    for frame in data.chunks_mut(channels) {
                        let f = o.next_sample().clamp(-1.0, 1.0);
                        let v = ((f * 0.5 + 0.5) * 65535.0) as u16;
                        for ch in frame.iter_mut() {
                            *ch = v;
                        }
                    }
                },
                on_err,
                None,
            )
        }
        other => return Err(format!("unsupported output format: {other:?}")),
    }
        .map_err(|e| format!("output build: {e}"))?;

    stream.play().map_err(|e| format!("output play: {e}"))?;
    Ok(stream)
}

/// Build + start an input stream on `device`, feeding the decoder.
fn build_input(
    device: &cpal::Device,
    tx_ui: &Sender<AppMessage>,
    detect_sf: &Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let supported = device
        .default_input_config()
        .map_err(|e| format!("input config: {e}"))?;
    let fmt = supported.sample_format();
    let config = low_latency_config(device, &supported, false);
    let sr = config.sample_rate as f32;
    let channels = (config.channels as usize).max(1);
    let latency_ms = estimate_latency_ms(&config, sr);
    let _ = tx_ui.send(AppMessage::StreamLatency { output: false, ms: latency_ms });
    // sized once up front so the per-callback scratch buffers below don't
    // need to grow (and thus reallocate) on their first use
    let scratch_cap = match config.buffer_size {
        cpal::BufferSize::Fixed(n) => n as usize * channels,
        cpal::BufferSize::Default => 2048,
    };

    let err_tx = tx_ui.clone();
    let on_err = move |e| {
        let _ = err_tx.send(AppMessage::AudioError(format!("RX stream: {e}")));
    };

    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let mut decoder = ToneDecoder::new(sr, Arc::clone(detect_sf));
            let tx = tx_ui.clone();
            let mut mono_buf = Vec::with_capacity(scratch_cap);
            let mut wave_buf = Vec::with_capacity(256);
            device.build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    downmix_into(data, channels, |s| s, &mut mono_buf);
                    emit(&mut decoder, &mono_buf, &mut wave_buf, &tx);
                },
                on_err,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let mut decoder = ToneDecoder::new(sr, Arc::clone(detect_sf));
            let tx = tx_ui.clone();
            let mut mono_buf = Vec::with_capacity(scratch_cap);
            let mut wave_buf = Vec::with_capacity(256);
            device.build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    downmix_into(data, channels, |s| s as f32 / 32768.0, &mut mono_buf);
                    emit(&mut decoder, &mono_buf, &mut wave_buf, &tx);
                },
                on_err,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let mut decoder = ToneDecoder::new(sr, Arc::clone(detect_sf));
            let tx = tx_ui.clone();
            let mut mono_buf = Vec::with_capacity(scratch_cap);
            let mut wave_buf = Vec::with_capacity(256);
            device.build_input_stream(
                config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    downmix_into(data, channels, |s| (s as f32 - 32768.0) / 32768.0, &mut mono_buf);
                    emit(&mut decoder, &mono_buf, &mut wave_buf, &tx);
                },
                on_err,
                None,
            )
        }
        other => return Err(format!("unsupported input format: {other:?}")),
    }
        .map_err(|e| format!("input build: {e}"))?;

    stream.play().map_err(|e| format!("input play: {e}"))?;
    Ok(stream)
}

pub fn run_audio_engine(tx_ui: Sender<AppMessage>, rx_audio: Receiver<AppMessage>) {
    let host = cpal::default_host();

    let tx_state = Arc::new(Mutex::new(TxOscillator {
        phase_1: 0.0,
        phase_2: 0.0,
        freq_1: 0.0,
        freq_2: 0.0,
        sample_rate: 48_000.0,
        frames_remaining: 0,
    }));
    // lives here (not inside a per-stream ToneDecoder) so the SF toggle
    // survives device rebuilds and rebuilding streams doesn't lose it
    let detect_sf = Arc::new(AtomicBool::new(false));

    // ---- initial streams on the default devices ----
    let mut output_stream: Option<cpal::Stream> = match host.default_output_device() {
        Some(dev) => match build_output(&dev, &tx_state, &tx_ui) {
            Ok(s) => Some(s),
            Err(e) => {
                let _ = tx_ui.send(AppMessage::AudioError(e));
                None
            }
        },
        None => {
            let _ = tx_ui.send(AppMessage::AudioError("no output device".into()));
            None
        }
    };

    let mut input_stream: Option<cpal::Stream> = match host.default_input_device() {
        Some(dev) => match build_input(&dev, &tx_ui, &detect_sf) {
            Ok(s) => Some(s),
            Err(e) => {
                let _ = tx_ui.send(AppMessage::AudioError(e));
                None
            }
        },
        None => None,
    };

    // ---- command loop ----
    while let Ok(msg) = rx_audio.recv() {
        match msg {
            AppMessage::SetOutputDevice(name) => {
                output_stream = None; // drop the old stream first
                match find_output_device(&host, &name) {
                    Some(dev) => match build_output(&dev, &tx_state, &tx_ui) {
                        Ok(s) => {
                            output_stream = Some(s);
                            let _ = tx_ui.send(AppMessage::AudioStatus(format!("TX -> {name}")));
                        }
                        Err(e) => {
                            let _ = tx_ui.send(AppMessage::AudioError(e));
                        }
                    },
                    None => {
                        let _ = tx_ui
                            .send(AppMessage::AudioError(format!("TX device not found: {name}")));
                    }
                }
            }
            AppMessage::SetInputDevice(name) => {
                input_stream = None;
                match find_input_device(&host, &name) {
                    Some(dev) => match build_input(&dev, &tx_ui, &detect_sf) {
                        Ok(s) => {
                            input_stream = Some(s);
                            let _ = tx_ui.send(AppMessage::AudioStatus(format!("RX <- {name}")));
                        }
                        Err(e) => {
                            let _ = tx_ui.send(AppMessage::AudioError(e));
                        }
                    },
                    None => {
                        let _ = tx_ui
                            .send(AppMessage::AudioError(format!("RX device not found: {name}")));
                    }
                }
            }
            AppMessage::SetDetectSf(enabled) => {
                detect_sf.store(enabled, std::sync::atomic::Ordering::Relaxed);
            }
            AppMessage::PlayTone { f1, f2, ms } => {
                let mut s = tx_state.lock().unwrap_or_else(|e| e.into_inner());
                s.phase_1 = 0.0;
                s.phase_2 = 0.0;
                s.freq_1 = f1;
                s.freq_2 = f2;
                s.frames_remaining = (s.sample_rate * (ms as f32 / 1000.0)) as usize;
            }
            AppMessage::StopAllTones => {
                let mut s = tx_state.lock().unwrap_or_else(|e| e.into_inner());
                s.frames_remaining = 0;
            }
            _ => {}
        }
    }

    // hold streams alive until the command channel closes
    drop(output_stream);
    drop(input_stream);
}