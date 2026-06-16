//! ZDL-Echo audio engine — TX Transmitter & RX Capture Engine.

#![allow(unused_assignments)]
#![allow(unused_variables)]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use crate::types::AppMessage;
use crate::decoder::ToneDecoder;

struct TxOscillator {
    phase_1: f32, phase_2: f32,
    freq_1: f32, freq_2: f32,
    sample_rate: f32,
    frames_remaining: usize,
}

pub fn run_audio_engine(tx_ui: Sender<AppMessage>, rx_audio: Receiver<AppMessage>) {
    let host = cpal::default_host();

    // --- TX PIPELINE ---
    let output_device = match host.default_output_device() {
        Some(d) => d,
        None => {
            let _ = tx_ui.send(AppMessage::AudioError("no audio output device".into()));
            return;
        }
    };

    let output_config: cpal::StreamConfig = output_device.default_output_config().unwrap().into();
    let sample_rate_out = output_config.sample_rate as f32;
    let out_channels = output_config.channels as usize;

    let tx_state = Arc::new(Mutex::new(TxOscillator {
        phase_1: 0.0, phase_2: 0.0, freq_1: 0.0, freq_2: 0.0,
        sample_rate: sample_rate_out, frames_remaining: 0,
    }));

    let tx_state_cb = Arc::clone(&tx_state);
    let tx_out_err = tx_ui.clone();

    let output_stream = output_device.build_output_stream(
        output_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut state = tx_state_cb.lock().unwrap();
            for frame in data.chunks_mut(out_channels) {
                let sample = if state.frames_remaining > 0 {
                    state.frames_remaining -= 1;
                    let s1 = (state.phase_1 * 2.0 * std::f32::consts::PI).sin();
                    state.phase_1 = (state.phase_1 + state.freq_1 / state.sample_rate) % 1.0;
                    if state.freq_2 > 0.0 {
                        let s2 = (state.phase_2 * 2.0 * std::f32::consts::PI).sin();
                        state.phase_2 = (state.phase_2 + state.freq_2 / state.sample_rate) % 1.0;
                        (s1 + s2) * 0.25
                    } else { s1 * 0.4 }
                } else { 0.0 };
                for channel_sample in frame.iter_mut() { *channel_sample = sample; }
            }
        },
        move |err| { let _ = tx_out_err.send(AppMessage::AudioError(format!("TX error: {err}"))); },
        None,
    ).unwrap();
    output_stream.play().unwrap();

    // --- RX PIPELINE ---
    let mut _active_input_stream: Option<cpal::Stream> = None;

    macro_rules! build_input {
        ($device:expr) => {{
            let config: cpal::StreamConfig = $device.default_input_config().unwrap().into();
            let sample_rate = config.sample_rate as f32;
            let channels = config.channels as usize;
            let mut decoder = ToneDecoder::new(sample_rate);
            let tx_in_data = tx_ui.clone();
            let tx_in_err = tx_ui.clone();

            let stream = $device.build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let ch = channels.max(1);
                    let mono: Vec<f32> =
                        data.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();

                    // Report input level so the UI meter proves audio is arriving.
                    let peak = mono.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
                    let _ = tx_in_data.send(AppMessage::RxLevel(peak));

                    for (_, ch) in decoder.process_samples(&mono) {
                        let _ = tx_in_data.send(AppMessage::DetectedTone(ch));
                    }
                },
                move |err| { let _ = tx_in_err.send(AppMessage::AudioError(format!("RX err: {err}"))); },
                None
            ).unwrap();
            stream.play().unwrap();
            stream
        }}
    }

    if let Some(in_dev) = host.default_input_device() {
        _active_input_stream = Some(build_input!(in_dev));
    }

    // --- COMMAND LOOP ---
    while let Ok(msg) = rx_audio.recv() {
        match msg {
            AppMessage::SetInputDevice(name) => {
                _active_input_stream = None;
                let mut found = false;
                if let Ok(devices) = host.input_devices() {
                    for dev in devices {
                        // cpal 0.18.1: Device implements Display, so to_string() is the name.
                        if dev.to_string() == name {
                            _active_input_stream = Some(build_input!(dev));
                            let _ = tx_ui.send(AppMessage::AudioStatus(format!("Hooked to: {}", name)));
                            found = true;
                            break;
                        }
                    }
                }
                if !found { let _ = tx_ui.send(AppMessage::AudioError(format!("Failed to hook: {}", name))); }
            }
            AppMessage::PlayTone { f1, f2, ms } => {
                let mut state = tx_state.lock().unwrap();
                state.phase_1 = 0.0; state.phase_2 = 0.0;
                state.freq_1 = f1; state.freq_2 = f2;
                state.frames_remaining = (state.sample_rate * (ms as f32 / 1000.0)) as usize;
            }
            AppMessage::StopAllTones => { tx_state.lock().unwrap().frames_remaining = 0; }
            _ => {}
        }
    }
}