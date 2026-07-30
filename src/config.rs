//! Persisted user settings and the TX/RX log export path.
//!
//! No serde in the dependency tree and the settings surface is tiny (a
//! handful of scalar fields), so this is a hand-rolled `key=value` text file
//! rather than pulling in a serialization crate.

use std::path::PathBuf;

pub struct Settings {
    pub input_device: String,
    pub output_device: String,
    pub mode_mf: bool,
    pub tone_ms: u32,
    pub detect_sf: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            input_device: String::new(),
            output_device: String::new(),
            mode_mf: false,
            tone_ms: 120,
            detect_sf: false,
        }
    }
}

/// Per-OS base directory for ZDL-Echo's config/log files. `None` if the
/// platform gives us no usable home/config env var to anchor to.
fn app_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("ZDL-Echo"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/ZDL-Echo"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            Some(PathBuf::from(xdg).join("zdl-echo"))
        } else {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/zdl-echo"))
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("settings.cfg"))
}

pub fn load() -> Settings {
    let mut s = Settings::default();
    let Some(path) = settings_path() else { return s };
    let Ok(text) = std::fs::read_to_string(path) else { return s };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "input_device" => s.input_device = v.to_string(),
            "output_device" => s.output_device = v.to_string(),
            "mode" => s.mode_mf = v == "MF",
            "tone_ms" => s.tone_ms = v.parse().unwrap_or(120).clamp(40, 600),
            "detect_sf" => s.detect_sf = v == "true",
            _ => {}
        }
    }
    s
}

/// Best-effort — a failure to persist settings (read-only filesystem, no
/// home dir, etc.) shouldn't interrupt using the app.
pub fn save(s: &Settings) {
    let Some(dir) = app_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let text = format!(
        "input_device={}\noutput_device={}\nmode={}\ntone_ms={}\ndetect_sf={}\n",
        s.input_device,
        s.output_device,
        if s.mode_mf { "MF" } else { "DTMF" },
        s.tone_ms,
        s.detect_sf,
    );
    let _ = std::fs::write(dir.join("settings.cfg"), text);
}

/// Write the log text to a timestamped file under the app's log directory.
/// Returns the path on success so the UI can show it.
pub fn export_log(text: &str) -> Result<PathBuf, String> {
    let dir = app_dir().ok_or("no writable config directory on this platform")?.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("zdl-echo-{}.log", format_utc_timestamp(secs)));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

/// `unix_secs` -> `YYYYMMDD-HHMMSS` (UTC). Hand-rolled civil-from-days
/// conversion (Howard Hinnant's algorithm) so this doesn't need a date/time
/// crate just to name a log file.
fn format_utc_timestamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs_of_day = unix_secs % 86_400;
    let (h, m, s) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m_num = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m_num <= 2 { y + 1 } else { y };

    format!("{y:04}{m_num:02}{d:02}-{h:02}{m:02}{s:02}")
}
