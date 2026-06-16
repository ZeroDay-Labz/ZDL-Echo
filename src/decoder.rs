//! ZDL-Echo Core DSP Engine

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneType { Dtmf, Mf, Sf }

pub struct ToneDecoder {
    sample_rate: f32,
    block_size: usize,
    sample_buffer: Vec<f32>,
    min_magnitude: f32,
    max_twist_db: f32,
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
            block_size: (sample_rate * 0.020) as usize,
            sample_buffer: Vec::with_capacity((sample_rate * 0.020) as usize),
            min_magnitude: 0.5,       // HYPER-SENSITIVE: lowered from 1.5 to catch quiet cables
            max_twist_db: 10.0,       // Tolerates slight distortion from virtual software cables
            drift_allowance: 0.015,
            last_detected: None,
            consecutive_hits: 0,
            required_hits: 1,         // Immediate reaction
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
                        if self.consecutive_hits == self.required_hits {
                            detected_events.push((tone_type, ch));
                        }
                    } else {
                        self.last_detected = Some((tone_type, ch));
                        self.consecutive_hits = 1;
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
        let total_energy: f32 = self.sample_buffer.iter().map(|&x| x * x).sum();
        let avg_energy = total_energy / (self.block_size as f32);
        if avg_energy < 0.000001 { return None; } // Lowered squelch threshold

        let sf_mag = self.goertzel_with_drift(2600.0);
        if sf_mag > self.min_magnitude && sf_mag > avg_energy * 10.0 {
            return Some((ToneType::Sf, '⌁'));
        }

        let dtmf_rows = [697.0, 770.0, 852.0, 941.0];
        let dtmf_cols = [1209.0, 1336.0, 1477.0, 1633.0];
        let dtmf_matrix = [['1', '2', '3', 'A'],['4', '5', '6', 'B'],['7', '8', '9', 'C'],['*', '0', '#', 'D']];

        let row_mags: Vec<f32> = dtmf_rows.iter().map(|&f| self.goertzel_with_drift(f)).collect();
        let col_mags: Vec<f32> = dtmf_cols.iter().map(|&f| self.goertzel_with_drift(f)).collect();

        if let (Some(r_idx), Some(c_idx)) = (find_strongest(&row_mags), find_strongest(&col_mags)) {
            if row_mags[r_idx] > self.min_magnitude && col_mags[c_idx] > self.min_magnitude {
                if self.validate_twist(row_mags[r_idx], col_mags[c_idx]) {
                    return Some((ToneType::Dtmf, dtmf_matrix[r_idx][c_idx]));
                }
            }
        }

        let mf_freqs = [700.0, 900.0, 1100.0, 1300.0, 1500.0, 1700.0];
        let mf_mags: Vec<f32> = mf_freqs.iter().map(|&f| self.goertzel_with_drift(f)).collect();

        if let Some((idx1, idx2)) = find_top_two(&mf_mags) {
            if mf_mags[idx1] > self.min_magnitude && mf_mags[idx2] > self.min_magnitude {
                if self.validate_twist(mf_mags[idx1], mf_mags[idx2]) {
                    if let Some(mf_char) = decode_mf(mf_freqs[idx1], mf_freqs[idx2]) {
                        return Some((ToneType::Mf, mf_char));
                    }
                }
            }
        }
        None
    }

    fn goertzel_with_drift(&self, target_freq: f32) -> f32 {
        let lower_bound = target_freq * (1.0 - self.drift_allowance);
        let upper_bound = target_freq * (1.0 + self.drift_allowance);
        let mag_center = self.goertzel(target_freq);
        let mag_lower = self.goertzel(lower_bound);
        let mag_upper = self.goertzel(upper_bound);
        mag_center.max(mag_lower).max(mag_upper)
    }

    fn goertzel(&self, target_freq: f32) -> f32 {
        let n = self.block_size as f32;
        let k = (n * target_freq / self.sample_rate).round();
        let omega = (2.0 * std::f32::consts::PI * k) / n;
        let cosine = omega.cos();
        let coeff = 2.0 * cosine;
        let mut q1 = 0.0; let mut q2 = 0.0;

        for &sample in &self.sample_buffer {
            let q0 = coeff * q1 - q2 + sample;
            q2 = q1; q1 = q0;
        }
        let magnitude_sq = (q1 * q1) + (q2 * q2) - (q1 * q2 * coeff);
        if magnitude_sq > 0.0 { magnitude_sq.sqrt() } else { 0.0 }
    }

    fn validate_twist(&self, mag1: f32, mag2: f32) -> bool {
        let ratio = if mag1 > mag2 { mag1 / mag2 } else { mag2 / mag1 };
        let twist_db = 20.0 * ratio.log10();
        twist_db <= self.max_twist_db
    }
}

fn find_strongest(mags: &[f32]) -> Option<usize> {
    mags.iter().enumerate().max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)).map(|(idx, _)| idx)
}

fn find_top_two(mags: &[f32]) -> Option<(usize, usize)> {
    if mags.len() < 2 { return None; }
    let mut indices: Vec<usize> = (0..mags.len()).collect();
    indices.sort_by(|&a, &b| mags[b].partial_cmp(&mags[a]).unwrap_or(std::cmp::Ordering::Equal));
    Some((indices[0], indices[1]))
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