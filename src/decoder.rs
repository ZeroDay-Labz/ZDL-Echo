//! ZDL-Echo Core DSP Engine
//!
//! Tone detection that rejects speech and noise. The key idea is *coherence*:
//! for a candidate frequency we measure what fraction of the block's total
//! energy is aligned with that frequency (0..1). A real DTMF/MF tone puts a
//! large share of energy into one or two narrow bins; speech spreads its energy
//! broadly, so every bin's coherence stays low. We only accept a detection when
//! the candidate tones genuinely dominate the block.

// ---- tunables ----
const SQUELCH: f32 = 1e-5;     // mean-power floor; below this the block is "silence"
const TONE_MIN: f32 = 0.06;    // each tone must hold >= 6% of block energy
const PAIR_MIN: f32 = 0.28;    // the two tones together must hold >= 28%
const DOMINANCE: f32 = 1.8;    // winner must beat the runner-up in its group by this factor
const MAX_TWIST_DB: f32 = 8.0; // allowed level difference between the two tones
const SF_MIN: f32 = 0.45;      // 2600 must hold >= 45% (a pure tone is ~0.5)
/// Detect the 2600 Hz single-frequency tone on RX. OFF by default — it is the
/// biggest source of false hits on speech. Flip to `true` to re-enable it
/// (with the strict SF_MIN gate above).
const DETECT_SF: bool = false;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneType { Dtmf, Mf, Sf }

pub struct ToneDecoder {
    sample_rate: f32,
    block_size: usize,
    sample_buffer: Vec<f32>,
    drift_allowance: f32,
    last_detected: Option<(ToneType, char)>,
    consecutive_hits: u32,
    required_hits: u32,
    silence_count: u32,
    required_silence: u32,
}

impl ToneDecoder {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            block_size: (sample_rate * 0.020) as usize, // 20 ms blocks
            sample_buffer: Vec::with_capacity((sample_rate * 0.020) as usize),
            drift_allowance: 0.015,
            last_detected: None,
            consecutive_hits: 0,
            required_hits: 2, // confirm over ~40 ms so transient speech can't fake a tone
            silence_count: 0,
            required_silence: 1,
        }
    }

    pub fn process_samples(&mut self, samples: &[f32]) -> Vec<(ToneType, char)> {
        let mut detected_events = Vec::new();

        for &sample in samples {
            self.sample_buffer.push(sample);

            if self.sample_buffer.len() >= self.block_size {
                if let Some((tone_type, ch)) = self.analyze_block() {
                    self.silence_count = 0;

                    if self.last_detected == Some((tone_type, ch)) {
                        self.consecutive_hits += 1;
                    } else {
                        self.last_detected = Some((tone_type, ch));
                        self.consecutive_hits = 1;
                    }

                    if self.consecutive_hits == self.required_hits {
                        detected_events.push((tone_type, ch));
                    }
                } else {
                    self.silence_count += 1;
                    if self.silence_count >= self.required_silence {
                        self.last_detected = None;
                        self.consecutive_hits = 0;
                    }
                }
                self.sample_buffer.clear();
            }
        }
        detected_events
    }

    fn analyze_block(&self) -> Option<(ToneType, char)> {
        let n = self.block_size as f32;
        let energy: f32 = self.sample_buffer.iter().map(|&x| x * x).sum();
        if energy / n < SQUELCH {
            return None; // silence — also avoids divide-by-tiny in coherence
        }

        // fraction of block energy aligned with frequency f, clamped to [0,1]
        let coh = |f: f32| -> f32 {
            let m = self.goertzel_with_drift(f);
            let c = (m * m) / (n * energy);
            if c.is_finite() { c.min(1.0) } else { 0.0 }
        };

        // ---- DTMF: one row tone + one column tone ----
        let rows = [697.0, 770.0, 852.0, 941.0];
        let cols = [1209.0, 1336.0, 1477.0, 1633.0];
        let matrix = [
            ['1', '2', '3', 'A'],
            ['4', '5', '6', 'B'],
            ['7', '8', '9', 'C'],
            ['*', '0', '#', 'D'],
        ];
        let rc: Vec<f32> = rows.iter().map(|&f| coh(f)).collect();
        let cc: Vec<f32> = cols.iter().map(|&f| coh(f)).collect();
        if let (Some((ri, rv)), Some((ci, cv))) = (top1(&rc), top1(&cc)) {
            let row_ok = rv > TONE_MIN && rv >= DOMINANCE * second(&rc);
            let col_ok = cv > TONE_MIN && cv >= DOMINANCE * second(&cc);
            if row_ok && col_ok && rv + cv > PAIR_MIN && twist_ok(rv, cv) {
                return Some((ToneType::Dtmf, matrix[ri][ci]));
            }
        }

        // ---- MF: two of six tones ----
        let mf = [700.0, 900.0, 1100.0, 1300.0, 1500.0, 1700.0];
        let mc: Vec<f32> = mf.iter().map(|&f| coh(f)).collect();
        if let Some((i1, v1, i2, v2)) = top2(&mc) {
            let dom = v1 > TONE_MIN && v2 > TONE_MIN && v2 >= DOMINANCE * third(&mc, i1, i2);
            if dom && v1 + v2 > PAIR_MIN && twist_ok(v1, v2) {
                if let Some(ch) = decode_mf(mf[i1], mf[i2]) {
                    return Some((ToneType::Mf, ch));
                }
            }
        }

        // ---- SF: single 2600 Hz tone ----
        if DETECT_SF {
            if coh(2600.0) > SF_MIN {
                return Some((ToneType::Sf, '⌁'));
            }
        }

        None
    }

    fn goertzel_with_drift(&self, target_freq: f32) -> f32 {
        let lower = target_freq * (1.0 - self.drift_allowance);
        let upper = target_freq * (1.0 + self.drift_allowance);
        self.goertzel(target_freq)
            .max(self.goertzel(lower))
            .max(self.goertzel(upper))
    }

    fn goertzel(&self, target_freq: f32) -> f32 {
        let n = self.block_size as f32;
        let k = (n * target_freq / self.sample_rate).round();
        let omega = (2.0 * std::f32::consts::PI * k) / n;
        let coeff = 2.0 * omega.cos();
        let mut q1 = 0.0;
        let mut q2 = 0.0;
        for &sample in &self.sample_buffer {
            let q0 = coeff * q1 - q2 + sample;
            q2 = q1;
            q1 = q0;
        }
        let mag_sq = (q1 * q1) + (q2 * q2) - (q1 * q2 * coeff);
        if mag_sq > 0.0 { mag_sq.sqrt() } else { 0.0 }
    }
}

fn top1(v: &[f32]) -> Option<(usize, f32)> {
    v.iter()
        .enumerate()
        .fold(None, |acc, (i, &x)| match acc {
            Some((_, best)) if best >= x => acc,
            _ => Some((i, x)),
        })
}

/// Second-largest value in the slice (0.0 if fewer than two entries).
fn second(v: &[f32]) -> f32 {
    let mut max = f32::MIN;
    let mut sec = f32::MIN;
    for &x in v {
        if x > max {
            sec = max;
            max = x;
        } else if x > sec {
            sec = x;
        }
    }
    if sec == f32::MIN { 0.0 } else { sec.max(0.0) }
}

/// Indices/values of the two largest entries, v1 >= v2.
fn top2(v: &[f32]) -> Option<(usize, f32, usize, f32)> {
    if v.len() < 2 {
        return None;
    }
    let mut i1 = 0;
    let mut v1 = f32::MIN;
    let mut i2 = 0;
    let mut v2 = f32::MIN;
    for (i, &x) in v.iter().enumerate() {
        if x > v1 {
            i2 = i1;
            v2 = v1;
            i1 = i;
            v1 = x;
        } else if x > v2 {
            i2 = i;
            v2 = x;
        }
    }
    Some((i1, v1.max(0.0), i2, v2.max(0.0)))
}

/// Largest value excluding two indices.
fn third(v: &[f32], skip1: usize, skip2: usize) -> f32 {
    let mut m = 0.0f32;
    for (i, &x) in v.iter().enumerate() {
        if i == skip1 || i == skip2 {
            continue;
        }
        if x > m {
            m = x;
        }
    }
    m
}

fn twist_ok(a: f32, b: f32) -> bool {
    if a <= 0.0 || b <= 0.0 {
        return false;
    }
    let ratio = if a > b { a / b } else { b / a };
    20.0 * ratio.log10() <= MAX_TWIST_DB
}

fn decode_mf(f1: f32, f2: f32) -> Option<char> {
    let mut pair = [f1 as u32, f2 as u32];
    pair.sort();
    match pair {
        [700, 900] => Some('1'), [700, 1100] => Some('2'), [900, 1100] => Some('3'),
        [700, 1300] => Some('4'), [900, 1300] => Some('5'), [1100, 1300] => Some('6'),
        [700, 1500] => Some('7'), [900, 1500] => Some('8'), [1100, 1500] => Some('9'),
        [1300, 1500] => Some('0'), [1100, 1700] => Some('['), [1500, 1700] => Some(']'),
        [1300, 1700] => Some('{'), _ => None,
    }
}