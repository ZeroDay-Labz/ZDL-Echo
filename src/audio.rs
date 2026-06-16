//! ZDL-Echo audio engine — TX transmitter & RX capture.
//!
//! Both the output (transmit) and input (capture) devices are selectable and
//! rebuilt on command. Every device/stream call is fallible: on failure we send
//! an AudioError to the UI and keep the engine alive instead of panicking, and
//! we convert i16/u16 hardware formats to/from f32 so non-float devices work.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use crate::types::AppMessage;
use crate::decoder::ToneDecoder;

const PI: f32 = std::f32::consts::PI;

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

/// Downmix interleaved frames of `T` to mono f32 via `conv`.
fn downmix<T: Copy>(data: &[T], channels: usize, conv: impl Fn(T) -> f32) -> Vec<f32> {
    let ch = channels.max(1);
    data.chunks(ch)
        .map(|frame| frame.iter().map(|&s| conv(s)).sum::<f32>() / ch as f32)
        .collect()
}

fn emit(decoder: &mut ToneDecoder, mono: &[f32], tx: &Sender<AppMessage>) {
    let peak = mono.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    let _ = tx.send(AppMessage::RxLevel(peak));

    // scope snapshot: decimate the buffer to ~256 points across its span
    const TARGET: usize = 256;
    let wave: Vec<f32> = if mono.len() <= TARGET {
        mono.to_vec()
    } else {
        let step = (mono.len() / TARGET).max(1);
        mono.iter().step_by(step).take(TARGET).copied().collect()
    };
    let _ = tx.send(AppMessage::RxWaveform(wave));

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
    let config: cpal::StreamConfig = supported.into();
    let sr = config.sample_rate as f32;
    let channels = (config.channels as usize).max(1);

    // keep the oscillator's clock matched to this device
    if let Ok(mut s) = tx_state.lock() {
        s.sample_rate = sr;
    }

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
                    let mut o = st.lock().unwrap();
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
                    let mut o = st.lock().unwrap();
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
                    let mut o = st.lock().unwrap();
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
) -> Result<cpal::Stream, String> {
    let supported = device
        .default_input_config()
        .map_err(|e| format!("input config: {e}"))?;
    let fmt = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let sr = config.sample_rate as f32;
    let channels = (config.channels as usize).max(1);

    let err_tx = tx_ui.clone();
    let on_err = move |e| {
        let _ = err_tx.send(AppMessage::AudioError(format!("RX stream: {e}")));
    };

    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let mut decoder = ToneDecoder::new(sr);
            let tx = tx_ui.clone();
            device.build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mono = downmix(data, channels, |s| s);
                    emit(&mut decoder, &mono, &tx);
                },
                on_err,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let mut decoder = ToneDecoder::new(sr);
            let tx = tx_ui.clone();
            device.build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let mono = downmix(data, channels, |s| s as f32 / 32768.0);
                    emit(&mut decoder, &mono, &tx);
                },
                on_err,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let mut decoder = ToneDecoder::new(sr);
            let tx = tx_ui.clone();
            device.build_input_stream(
                config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let mono = downmix(data, channels, |s| (s as f32 - 32768.0) / 32768.0);
                    emit(&mut decoder, &mono, &tx);
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
        Some(dev) => match build_input(&dev, &tx_ui) {
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
                    Some(dev) => match build_input(&dev, &tx_ui) {
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
            AppMessage::PlayTone { f1, f2, ms } => {
                if let Ok(mut s) = tx_state.lock() {
                    s.phase_1 = 0.0;
                    s.phase_2 = 0.0;
                    s.freq_1 = f1;
                    s.freq_2 = f2;
                    s.frames_remaining = (s.sample_rate * (ms as f32 / 1000.0)) as usize;
                }
            }
            AppMessage::StopAllTones => {
                if let Ok(mut s) = tx_state.lock() {
                    s.frames_remaining = 0;
                }
            }
            _ => {}
        }
    }

    // hold streams alive until the command channel closes
    drop(output_stream);
    drop(input_stream);
}