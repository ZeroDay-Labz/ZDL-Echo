#![allow(deprecated)]

use eframe::egui;
use crossbeam_channel::{Receiver, Sender};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use cpal::traits::HostTrait;
use crate::types::AppMessage;
use crate::generator::{get_dtmf_freqs, get_mf_freqs};

// ---- phosphor palette ----
const BG: egui::Color32        = egui::Color32::from_rgb(8, 8, 8);
const PANEL: egui::Color32     = egui::Color32::from_rgb(13, 13, 13);
const TERM_BG: egui::Color32   = egui::Color32::from_rgb(5, 5, 5);
const CHARCOAL: egui::Color32  = egui::Color32::from_rgb(22, 22, 22);
const BORDER: egui::Color32    = egui::Color32::from_rgb(40, 40, 40);
const GREEN: egui::Color32     = egui::Color32::from_rgb(0, 255, 90);
const DIM: egui::Color32       = egui::Color32::from_rgb(0, 110, 50);
const RED: egui::Color32       = egui::Color32::from_rgb(255, 70, 70);
const WHITE: egui::Color32     = egui::Color32::from_rgb(220, 220, 220);
// amber CRT accents — homage to the old monochrome amber monitors
const AMBER: egui::Color32     = egui::Color32::from_rgb(255, 176, 0);
const AMBER_DIM: egui::Color32 = egui::Color32::from_rgb(150, 100, 0);
const AMBER_SEL: egui::Color32 = egui::Color32::from_rgb(74, 52, 0); // selection bg — text stays readable

const DIAL_GAP_MS: u64 = 70;
/// Silence (seconds) that ends a captured/transmitted run and starts a new line.
const LINE_GAP_SECS: f32 = 3.0;

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Dtmf,
    Mf,
}

pub struct DtmfApp {
    tx: Sender<AppMessage>,
    rx: Receiver<AppMessage>,
    committed: String,        // finished lines (history)
    pend_tx: String,          // current TX run, not yet committed
    pend_rx: String,          // current RX run, not yet committed
    pend_tx_at: Option<Instant>,
    pend_rx_at: Option<Instant>,
    mode: Mode,
    tone_ms: u32,
    show_about: bool,
    show_routing: bool,

    input_devices: Vec<String>,
    selected_input: String,
    rx_level: f32,

    dial_input: String,
    dial_queue: VecDeque<char>,
    next_fire_at: Option<Instant>,
    tx_until: Option<Instant>,

    current_f1: f32,
    current_f2: f32,
}

impl DtmfApp {
    pub fn new(tx: Sender<AppMessage>, rx: Receiver<AppMessage>) -> Self {
        let host = cpal::default_host();
        let mut in_devs = Vec::new();
        if let Ok(devices) = host.input_devices() {
            for d in devices {
                in_devs.push(d.to_string());
            }
        }
        in_devs.dedup();

        let default_in = host
            .default_input_device()
            .map(|d| d.to_string())
            .unwrap_or_default();

        Self {
            tx,
            rx,
            committed: String::from("ZDL-ECHO // TONE TRANSMITTER\nready.\n"),
            pend_tx: String::new(),
            pend_rx: String::new(),
            pend_tx_at: None,
            pend_rx_at: None,
            mode: Mode::Dtmf,
            tone_ms: 120,
            show_about: false,
            show_routing: false,
            input_devices: in_devs,
            selected_input: default_in,
            rx_level: 0.0,
            dial_input: String::new(),
            dial_queue: VecDeque::new(),
            next_fire_at: None,
            tx_until: None,
            current_f1: 0.0,
            current_f2: 0.0,
        }
    }

    /// Add a tone fragment to the live run for its source. Consecutive tones
    /// accumulate on one line until LINE_GAP_SECS of silence commits them.
    fn push_tone(&mut self, frag: &str, is_rx: bool) {
        let now = Instant::now();
        if is_rx {
            self.pend_rx.push_str(frag);
            self.pend_rx_at = Some(now);
        } else {
            self.pend_tx.push_str(frag);
            self.pend_tx_at = Some(now);
        }
    }

    /// Write a discrete status line ([SYS]/[ERR]/[DIAL]). Flushes any in-flight
    /// runs first so a status message never lands mid-number.
    fn log_status(&mut self, line: &str) {
        self.flush_pending(true);
        if !self.committed.is_empty() && !self.committed.ends_with('\n') {
            self.committed.push('\n');
        }
        self.committed.push_str(line);
        if !self.committed.ends_with('\n') {
            self.committed.push('\n');
        }
        self.trim_committed();
    }

    /// Commit pending runs to history. `force` flushes regardless of timing;
    /// otherwise only runs idle for >= LINE_GAP_SECS are committed.
    fn flush_pending(&mut self, force: bool) {
        let now = Instant::now();
        if !self.pend_tx.is_empty() {
            let idle = self
                .pend_tx_at
                .map_or(true, |t| now.duration_since(t).as_secs_f32() >= LINE_GAP_SECS);
            if force || idle {
                self.committed.push_str(&format!("[TX] {}\n", self.pend_tx));
                self.pend_tx.clear();
                self.pend_tx_at = None;
            }
        }
        if !self.pend_rx.is_empty() {
            let idle = self
                .pend_rx_at
                .map_or(true, |t| now.duration_since(t).as_secs_f32() >= LINE_GAP_SECS);
            if force || idle {
                self.committed.push_str(&format!("[RX] {}\n", self.pend_rx));
                self.pend_rx.clear();
                self.pend_rx_at = None;
            }
        }
        self.trim_committed();
    }

    fn trim_committed(&mut self) {
        if self.committed.len() > 16_000 {
            let mut cut = self.committed.len() - 12_000;
            while cut < self.committed.len() && !self.committed.is_char_boundary(cut) {
                cut += 1;
            }
            self.committed.drain(0..cut);
        }
    }

    /// History plus the live (still-growing) TX and RX runs, for display.
    fn display_log(&self) -> String {
        let mut out = self.committed.clone();
        if !self.pend_tx.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("[TX] ");
            out.push_str(&self.pend_tx);
        }
        if !self.pend_rx.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("[RX] ");
            out.push_str(&self.pend_rx);
        }
        out
    }

    fn clear_log(&mut self) {
        self.committed.clear();
        self.pend_tx.clear();
        self.pend_rx.clear();
        self.pend_tx_at = None;
        self.pend_rx_at = None;
    }

    fn play(&mut self, f1: f32, f2: f32, label: &str) {
        let _ = self.tx.send(AppMessage::PlayTone { f1, f2, ms: self.tone_ms });
        self.tx_until = Some(Instant::now() + Duration::from_millis(self.tone_ms as u64));
        self.current_f1 = f1;
        self.current_f2 = f2;
        self.push_tone(label, false);
    }

    fn fire_key(&mut self, key: char) {
        let freqs = match self.mode {
            Mode::Dtmf => get_dtmf_freqs(key),
            Mode::Mf => {
                if key.is_ascii_digit() {
                    get_mf_freqs(&key.to_string())
                } else {
                    None
                }
            }
        };
        if let Some((f1, f2)) = freqs {
            self.play(f1, f2, &key.to_string());
        }
    }

    fn fire_mf(&mut self, key: &str) {
        if let Some((f1, f2)) = get_mf_freqs(key) {
            self.play(f1, f2, &format!("<{}>", key));
        }
    }

    fn fire_sf(&mut self) {
        self.play(2600.0, 0.0, "<2600>");
    }

    fn stop(&mut self) {
        let _ = self.tx.send(AppMessage::StopAllTones);
        self.dial_queue.clear();
        self.next_fire_at = None;
        self.tx_until = None;
        self.current_f1 = 0.0;
        self.current_f2 = 0.0;
        self.log_status("[SYS] transmit halted");
    }

    fn start_dial(&mut self) {
        let seq: VecDeque<char> = self
            .dial_input
            .chars()
            .map(|c| c.to_ascii_uppercase())
            .filter(|c| "0123456789ABCD*#".contains(*c))
            .collect();
        if seq.is_empty() {
            return;
        }
        self.log_status(&format!("[DIAL] {}", self.dial_input.trim()));
        self.dial_queue = seq;
        self.next_fire_at = Some(Instant::now());
    }

    fn pump_dial(&mut self) {
        if self.dial_queue.is_empty() {
            return;
        }
        let now = Instant::now();
        let ready = self.next_fire_at.map_or(true, |t| now >= t);
        if !ready {
            return;
        }
        if let Some(c) = self.dial_queue.pop_front() {
            self.fire_key(c);
            self.next_fire_at =
                Some(now + Duration::from_millis(self.tone_ms as u64 + DIAL_GAP_MS));
        }
        if self.dial_queue.is_empty() {
            self.next_fire_at = None;
        }
    }

    fn tx_active(&self) -> bool {
        self.tx_until.map_or(false, |t| Instant::now() < t)
    }

    /// Horizontal input-level bar. Green (quiet) -> amber -> red (hot).
    fn draw_level_meter(&self, ui: &mut egui::Ui, height: f32) {
        let w = ui.available_width();
        let (resp, painter) =
            ui.allocate_painter(egui::vec2(w, height), egui::Sense::hover());
        let r = resp.rect;
        painter.rect_filled(r, 0.0, egui::Color32::from_rgb(10, 10, 10));

        let lvl = self.rx_level.clamp(0.0, 1.0);
        let shown = lvl.sqrt(); // lift quiet signals so they're visible
        let fill_w = r.width() * shown;
        if fill_w > 1.0 {
            let col = if lvl > 0.5 {
                RED
            } else if lvl > 0.12 {
                AMBER
            } else {
                GREEN
            };
            let fill = egui::Rect::from_min_size(r.min, egui::vec2(fill_w, r.height()));
            painter.rect_filled(fill, 0.0, col);
        }
        painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    }

    fn apply_style(&self, ctx: &egui::Context) {
        let mut v = egui::Visuals::dark();
        v.window_fill = BG;
        v.panel_fill = BG;

        v.widgets.noninteractive.corner_radius = egui::CornerRadius::ZERO;
        v.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
        v.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
        v.widgets.active.corner_radius = egui::CornerRadius::ZERO;

        // selection in dark amber so highlighted text / items stay readable
        // (was solid green-on-green = invisible).
        v.selection.bg_fill = AMBER_SEL;
        v.selection.stroke = egui::Stroke::new(1.0, AMBER);

        v.widgets.hovered.bg_fill = CHARCOAL;
        v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, AMBER);
        v.widgets.active.bg_fill = AMBER;
        v.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::BLACK);

        ctx.set_visuals(v);

        let mut style = (*ctx.global_style()).clone();
        style.override_font_id = Some(egui::FontId::monospace(14.0));
        ctx.set_global_style(style);
    }
}

impl eframe::App for DtmfApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.apply_style(&ctx);

        let typing = ctx.wants_keyboard_input();
        let mut keys: Vec<char> = Vec::new();
        let mut kill = false;
        ctx.input_mut(|i| {
            if !typing && i.consume_key(egui::Modifiers::NONE, egui::Key::Space) {
                kill = true;
            }
            if !typing {
                for event in &i.events {
                    if let egui::Event::Text(t) = event {
                        for c in t.chars() {
                            let cu = c.to_ascii_uppercase();
                            if "0123456789ABCD*#".contains(cu) {
                                keys.push(cu);
                            }
                        }
                    }
                }
            }
        });
        if kill {
            self.stop();
        }
        for c in keys {
            self.fire_key(c);
        }

        self.pump_dial();

        // ---- drain audio-thread messages ----
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMessage::DetectedTone(c) => {
                    let s = if c == '⌁' { "[SF]".to_string() } else { c.to_string() };
                    self.push_tone(&s, true);
                }
                AppMessage::RxLevel(p) => {
                    if p > self.rx_level {
                        self.rx_level = p;
                    }
                }
                AppMessage::AudioStatus(m) => {
                    self.log_status(&format!("[SYS] {m}"));
                }
                AppMessage::AudioError(e) => {
                    self.log_status(&format!("[ERR] {e}"));
                }
                _ => {}
            }
        }
        // smooth peak-hold decay for the meter
        self.rx_level *= 0.85;
        // commit any run that's gone quiet for >= LINE_GAP_SECS
        self.flush_pending(false);

        let control_frame = egui::Frame::none()
            .fill(PANEL)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .inner_margin(12.0);
        let terminal_frame = egui::Frame::none()
            .fill(TERM_BG)
            .stroke(egui::Stroke::new(1.0, DIM))
            .inner_margin(14.0);

        // ---- top bar ----
        egui::TopBottomPanel::top("menu").show_inside(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("SYSTEM", |ui| {
                    if ui.button("about").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("terminate").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("ROUTING", |ui| {
                    if ui.button("Capture Source / Endpoints").clicked() {
                        self.show_routing = true;
                        ui.close();
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new("ZDL-ECHO").color(AMBER).strong());
                });
            });
        });

        // ---- control deck ----
        egui::SidePanel::left("controls")
            .exact_width(244.0)
            .frame(control_frame)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // RX signal presence
                    ui.label(egui::RichText::new("RX SIGNAL").color(AMBER_DIM).size(11.0));
                    self.draw_level_meter(ui, 16.0);
                    let src = if self.selected_input.is_empty() {
                        "— no source —".to_string()
                    } else {
                        self.selected_input.clone()
                    };
                    ui.label(egui::RichText::new(src).color(DIM).size(10.0));
                    ui.label(
                        egui::RichText::new("source > ROUTING menu")
                            .color(AMBER_DIM)
                            .size(9.0),
                    );

                    ui.add_space(14.0);

                    // MODE — dimmed, amber active
                    ui.label(egui::RichText::new("MODE").color(AMBER_DIM).size(11.0));
                    ui.horizontal(|ui| {
                        for (m, lbl) in [(Mode::Dtmf, "DTMF"), (Mode::Mf, "MF")] {
                            let active = self.mode == m;
                            let (txt, fill) = if active {
                                (egui::Color32::BLACK, AMBER)
                            } else {
                                (DIM, CHARCOAL)
                            };
                            if ui
                                .add_sized(
                                    [70.0, 26.0],
                                    egui::Button::new(egui::RichText::new(lbl).color(txt))
                                        .fill(fill),
                                )
                                .clicked()
                            {
                                self.mode = m;
                            }
                        }
                    });

                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("DURATION").color(AMBER_DIM).size(11.0));
                    ui.add(egui::Slider::new(&mut self.tone_ms, 40..=600).suffix(" ms"));

                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("DIAL STRING").color(AMBER_DIM).size(11.0));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.dial_input)
                            .hint_text("18005551234")
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let dialing = !self.dial_queue.is_empty();
                        if dialing {
                            if ui
                                .add_sized(
                                    [110.0, 28.0],
                                    egui::Button::new(egui::RichText::new("ABORT").color(WHITE))
                                        .fill(egui::Color32::from_rgb(60, 14, 14)),
                                )
                                .clicked()
                            {
                                self.stop();
                            }
                        } else if ui
                            .add_sized(
                                [110.0, 28.0],
                                egui::Button::new(egui::RichText::new("DIAL").color(GREEN))
                                    .fill(CHARCOAL),
                            )
                            .clicked()
                        {
                            self.start_dial();
                        }

                        let (dot, col, txt) = if self.tx_active() {
                            ("\u{25CF}", RED, "TX")
                        } else {
                            ("\u{25CB}", DIM, "idle")
                        };
                        ui.label(egui::RichText::new(format!("{dot} {txt}")).color(col));
                    });

                    ui.add_space(14.0);
                    ui.label(egui::RichText::new("SUPERVISORY").color(AMBER_DIM).size(11.0));
                    if ui
                        .add_sized(
                            [ui.available_width(), 36.0],
                            egui::Button::new(egui::RichText::new("2600 Hz").size(16.0).color(RED))
                                .fill(CHARCOAL),
                        )
                        .clicked()
                    {
                        self.fire_sf();
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.add_sized([70.0, 28.0], egui::Button::new("KP").fill(CHARCOAL)).clicked() {
                            self.fire_mf("KP");
                        }
                        if ui.add_sized([70.0, 28.0], egui::Button::new("KP2").fill(CHARCOAL)).clicked() {
                            self.fire_mf("KP2");
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.add_sized([45.0, 28.0], egui::Button::new("ST").fill(CHARCOAL)).clicked() {
                            self.fire_mf("ST");
                        }
                        if ui.add_sized([45.0, 28.0], egui::Button::new("ST2").fill(CHARCOAL)).clicked() {
                            self.fire_mf("ST2");
                        }
                        if ui.add_sized([45.0, 28.0], egui::Button::new("ST3").fill(CHARCOAL)).clicked() {
                            self.fire_mf("ST3");
                        }
                    });

                    ui.add_space(14.0);
                    ui.label(egui::RichText::new("KEYPAD").color(AMBER_DIM).size(11.0));
                    let matrix = [
                        ['1', '2', '3', 'A'],
                        ['4', '5', '6', 'B'],
                        ['7', '8', '9', 'C'],
                        ['*', '0', '#', 'D'],
                    ];
                    egui::Grid::new("keypad").spacing([6.0, 6.0]).show(ui, |ui| {
                        for row in matrix {
                            for key in row {
                                let enabled =
                                    matches!(self.mode, Mode::Dtmf) || key.is_ascii_digit();
                                let col = if enabled { GREEN } else { DIM };
                                let btn = egui::Button::new(
                                    egui::RichText::new(key.to_string()).size(19.0).color(col),
                                )
                                    .fill(CHARCOAL);
                                if ui.add_enabled(enabled, btn).clicked() {
                                    self.fire_key(key);
                                }
                            }
                            ui.end_row();
                        }
                    });

                    ui.add_space(14.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 34.0],
                            egui::Button::new(egui::RichText::new("STOP  (space)").color(WHITE))
                                .fill(egui::Color32::from_rgb(60, 14, 14)),
                        )
                        .clicked()
                    {
                        self.stop();
                    }
                });
            });

        // ---- central: scope + log ----
        egui::CentralPanel::default()
            .frame(terminal_frame)
            .show_inside(ui, |ui| {
                ui.heading(egui::RichText::new("SIGNAL OSCILLOSCOPE").color(AMBER));
                ui.separator();

                let osc_height = 100.0;
                let (response, painter) = ui.allocate_painter(
                    egui::vec2(ui.available_width(), osc_height),
                    egui::Sense::hover(),
                );
                let rect = response.rect;

                painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(8, 8, 8));
                painter.rect_stroke(
                    rect,
                    0.0,
                    egui::Stroke::new(1.0, BORDER),
                    egui::StrokeKind::Inside,
                );
                painter.line_segment(
                    [
                        egui::pos2(rect.left(), rect.center().y),
                        egui::pos2(rect.right(), rect.center().y),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(20, 40, 20)),
                );

                if self.tx_active() {
                    let time = ui.input(|i| i.time) as f32;
                    let width = rect.width();
                    let mut points = Vec::new();
                    for i in 0..width as i32 {
                        let x = i as f32;
                        let t = time * 4.0 + (x * 0.015);
                        let vis_f1 = self.current_f1 * 0.05;
                        let vis_f2 = self.current_f2 * 0.05;
                        let w1 = (t * vis_f1).sin();
                        let w2 = if self.current_f2 > 0.0 { (t * vis_f2).sin() } else { 0.0 };
                        let mixed = if self.current_f2 > 0.0 { (w1 + w2) * 0.5 } else { w1 };
                        let y = rect.center().y - (mixed * (rect.height() * 0.4));
                        points.push(egui::pos2(rect.left() + x, y));
                    }
                    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, GREEN)));

                    let freq_text = if self.current_f2 > 0.0 {
                        format!("{:.0} Hz + {:.0} Hz", self.current_f1, self.current_f2)
                    } else {
                        format!("{:.0} Hz (SF)", self.current_f1)
                    };
                    painter.text(
                        rect.left_top() + egui::vec2(8.0, 8.0),
                        egui::Align2::LEFT_TOP,
                        freq_text,
                        egui::FontId::monospace(14.0),
                        AMBER,
                    );
                } else {
                    painter.line_segment(
                        [
                            egui::pos2(rect.left(), rect.center().y),
                            egui::pos2(rect.right(), rect.center().y),
                        ],
                        egui::Stroke::new(2.0, DIM),
                    );
                    painter.text(
                        rect.left_top() + egui::vec2(8.0, 8.0),
                        egui::Align2::LEFT_TOP,
                        "IDLE",
                        egui::FontId::monospace(14.0),
                        AMBER_DIM,
                    );
                }

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    ui.heading(egui::RichText::new("TX/RX LOG").color(AMBER));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("clear").clicked() {
                            self.clear_log();
                        }
                    });
                });
                ui.separator();

                // read-only, selectable & copyable; scrolling up releases the
                // auto-stick so you can highlight history while tones arrive.
                let log = self.display_log();
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(log).monospace().color(GREEN),
                            )
                                .selectable(true),
                        );
                    });
            });

        // ---- routing / capture-source window ----
        if self.show_routing {
            let mut close_routing = false;
            // .open() must borrow a LOCAL, not a field of self — the closure below
            // needs &mut self, and borrowing self.show_routing here would collide.
            let mut keep_open = true;
            egui::Window::new("AUDIO ROUTING")
                .collapsible(false)
                .resizable(false)
                .open(&mut keep_open)
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgb(15, 15, 15)))
                .show(&ctx, |ui| {
                    ui.set_max_width(380.0);

                    ui.label(egui::RichText::new("RX CAPTURE SOURCE").color(AMBER).strong());
                    ui.label(
                        egui::RichText::new("live input level")
                            .color(AMBER_DIM)
                            .size(10.0),
                    );
                    self.draw_level_meter(ui, 20.0);

                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("ACTIVE:").color(AMBER_DIM).size(10.0));
                    let active = if self.selected_input.is_empty() {
                        "— none —".to_string()
                    } else {
                        self.selected_input.clone()
                    };
                    ui.label(egui::RichText::new(active).color(GREEN));

                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(egui::RichText::new("ENDPOINTS (click to hook)").color(AMBER_DIM).size(10.0));

                    let current = self.selected_input.clone();
                    let mut clicked: Option<String> = None;
                    egui::ScrollArea::vertical()
                        .max_height(240.0)
                        .show(ui, |ui| {
                            for dev in &self.input_devices {
                                let sel = *dev == current;
                                let color = if sel { AMBER } else { GREEN };
                                if ui
                                    .selectable_label(sel, egui::RichText::new(dev).color(color))
                                    .clicked()
                                {
                                    clicked = Some(dev.clone());
                                }
                            }
                        });
                    if let Some(d) = clicked {
                        self.selected_input = d.clone();
                        let _ = self.tx.send(AppMessage::SetInputDevice(d));
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(
                        egui::RichText::new(
                            "To capture a specific app, route it to a Voicemeeter virtual \
                             input in Voicemeeter, then pick that bus (e.g. \"Voicemeeter Out B1\") \
                             above. ZDL-Echo captures the device; Voicemeeter does the per-app routing.",
                        )
                            .color(AMBER_DIM),
                    );
                    ui.add_space(6.0);
                    if ui.button("[ CLOSE ]").clicked() {
                        close_routing = true;
                    }
                });
            if close_routing || !keep_open {
                self.show_routing = false;
            }
        }

        // ---- about window ----
        if self.show_about {
            let mut acknowledge_clicked = false;
            egui::Window::new("SYS_INFO")
                .collapsible(false)
                .resizable(false)
                .open(&mut self.show_about)
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgb(15, 15, 15)))
                .show(&ctx, |ui| {
                    ui.set_max_width(360.0);
                    ui.heading(egui::RichText::new("PROJECT: ZDL-ECHO").color(AMBER));
                    ui.label("TELECOM RESEARCH & SIGNALING TOOLKIT");
                    ui.separator();

                    ui.add_space(6.0);
                    ui.label("DTMF — touch-tone dialing");
                    ui.label("MF   — R1 inter-office (KP / digits / ST)");
                    ui.label("2600 — single-frequency supervisory");
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("space = stop all tones").color(egui::Color32::from_rgb(150, 150, 150)),
                    );
                    ui.add_space(10.0);
                    ui.separator();

                    ui.label("Created By: havok");
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("BROUGHT TO YOU BY ZERO DAY LABS")
                            .color(AMBER)
                            .strong(),
                    );
                    ui.add_space(10.0);
                    ui.label("WARNING: FOR AUTHORIZED DIAGNOSTICS ONLY.");
                    ui.separator();
                    if ui.button("[ ACKNOWLEDGE ]").clicked() {
                        acknowledge_clicked = true;
                    }
                });
            if acknowledge_clicked {
                self.show_about = false;
            }
        }

        let repaint = if self.tx_active() {
            Duration::from_millis(16)
        } else if !self.dial_queue.is_empty() {
            Duration::from_millis(8)
        } else {
            Duration::from_millis(33)
        };
        ctx.request_repaint_after(repaint);
    }
}