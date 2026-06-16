#![allow(deprecated)]

use eframe::egui;
use crossbeam_channel::{Receiver, Sender};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use cpal::traits::HostTrait; // Removed unused DeviceTrait
use crate::types::AppMessage;
use crate::generator::{get_dtmf_freqs, get_mf_freqs};

const BG: egui::Color32       = egui::Color32::from_rgb(8, 8, 8);
const PANEL: egui::Color32    = egui::Color32::from_rgb(13, 13, 13);
const TERM_BG: egui::Color32  = egui::Color32::from_rgb(5, 5, 5);
const CHARCOAL: egui::Color32 = egui::Color32::from_rgb(22, 22, 22);
const BORDER: egui::Color32   = egui::Color32::from_rgb(40, 40, 40);
const GREEN: egui::Color32    = egui::Color32::from_rgb(0, 255, 90);
const DIM: egui::Color32      = egui::Color32::from_rgb(0, 110, 50);
const RED: egui::Color32      = egui::Color32::from_rgb(255, 70, 70);
const WHITE: egui::Color32    = egui::Color32::from_rgb(220, 220, 220);

const DIAL_GAP_MS: u64 = 70;

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Dtmf,
    Mf,
}

pub struct DtmfApp {
    tx: Sender<AppMessage>,
    rx: Receiver<AppMessage>,
    log_text: String,
    mode: Mode,
    tone_ms: u32,
    show_about: bool,
    show_routing: bool,

    input_devices: Vec<String>,
    selected_input: String,

    dial_input: String,
    dial_queue: VecDeque<char>,
    next_fire_at: Option<Instant>,
    tx_until: Option<Instant>,

    current_f1: f32,
    current_f2: f32,

    last_activity_time: Option<Instant>,
    last_was_rx: Option<bool>,
}

impl DtmfApp {
    pub fn new(tx: Sender<AppMessage>, rx: Receiver<AppMessage>) -> Self {
        let host = cpal::default_host();
        let mut in_devs = Vec::new();

        // FIX: In CPAL 0.18.1, Device natively implements Display. No .name() needed.
        if let Ok(devices) = host.input_devices() {
            for d in devices {
                in_devs.push(d.to_string());
            }
        }
        in_devs.dedup();

        let default_in = host.default_input_device()
            .map(|d| d.to_string())
            .unwrap_or_default();

        Self {
            tx,
            rx,
            log_text: String::from("ZDL-ECHO // TONE TRANSMITTER\nready.\n"),
            mode: Mode::Dtmf,
            tone_ms: 120,
            show_about: false,
            show_routing: false,
            input_devices: in_devs,
            selected_input: default_in,
            dial_input: String::new(),
            dial_queue: VecDeque::new(),
            next_fire_at: None,
            tx_until: None,
            current_f1: 0.0,
            current_f2: 0.0,
            last_activity_time: None,
            last_was_rx: None,
        }
    }

    fn append_to_log(&mut self, text: &str, is_rx: bool) {
        let now = Instant::now();
        let gap = self.last_activity_time.map_or(100.0, |t| now.duration_since(t).as_secs_f32());
        let source_changed = self.last_was_rx != Some(is_rx);

        if gap > 3.0 || source_changed {
            if !self.log_text.is_empty() && !self.log_text.ends_with('\n') {
                self.log_text.push('\n');
            }
            let prefix = if is_rx { "\n[RX] " } else { "\n[TX] " };
            self.log_text.push_str(prefix);
        }

        self.log_text.push_str(text);

        if self.log_text.len() > 16_000 {
            let cut = self.log_text.len() - 12_000;
            self.log_text.drain(0..cut);
        }

        self.last_activity_time = Some(now);
        self.last_was_rx = Some(is_rx);
    }

    fn play(&mut self, f1: f32, f2: f32, label: &str) {
        let _ = self.tx.send(AppMessage::PlayTone { f1, f2, ms: self.tone_ms });
        self.tx_until = Some(Instant::now() + Duration::from_millis(self.tone_ms as u64));
        self.current_f1 = f1;
        self.current_f2 = f2;
        self.append_to_log(label, false);
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
        self.append_to_log("\n[SYS] transmit halted\n", false);
        self.last_activity_time = None;
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
        self.append_to_log(&format!("\n[DIAL] {}\n", self.dial_input.trim()), false);
        self.dial_queue = seq;
        self.next_fire_at = Some(Instant::now());
        self.last_activity_time = None;
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

    fn apply_style(&self, ctx: &egui::Context) {
        let mut v = egui::Visuals::dark();
        v.window_fill = BG;
        v.panel_fill = BG;
        v.widgets.noninteractive.corner_radius = egui::CornerRadius::ZERO;
        v.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
        v.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
        v.widgets.active.corner_radius = egui::CornerRadius::ZERO;
        v.selection.bg_fill = GREEN;
        v.widgets.hovered.bg_fill = CHARCOAL;
        v.widgets.active.bg_fill = GREEN;
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

        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMessage::DetectedTone(c) => {
                    self.append_to_log(&c.to_string(), true);
                },
                AppMessage::AudioStatus(msg) => {
                    self.log_text.push_str(&format!("\n[SYS] {msg}\n"));
                    self.last_activity_time = None;
                },
                AppMessage::AudioError(err) => {
                    self.log_text.push_str(&format!("\n[ERR] {err}\n"));
                    self.last_activity_time = None;
                },
                _ => {}
            }
        }

        let control_frame = egui::Frame::none()
            .fill(PANEL)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .inner_margin(12.0);
        let terminal_frame = egui::Frame::none()
            .fill(TERM_BG)
            .stroke(egui::Stroke::new(1.0, DIM))
            .inner_margin(14.0);

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
                    if ui.button("View Hardware Endpoints").clicked() {
                        self.show_routing = true;
                        ui.close();
                    }
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new("ZDL-ECHO").color(GREEN).strong());
                });
            });
        });

        egui::SidePanel::left("controls").exact_width(244.0).frame(control_frame).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {

                ui.label(egui::RichText::new("RX CAPTURE SOURCE").color(DIM).size(11.0));
                ui.scope(|ui| {
                    ui.visuals_mut().widgets.inactive.bg_fill = CHARCOAL;

                    let display_name = if self.selected_input.len() > 24 {
                        format!("{}...", &self.selected_input[..21])
                    } else {
                        self.selected_input.clone()
                    };

                    egui::ComboBox::from_id_source("rx_source")
                        .selected_text(display_name)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for dev in &self.input_devices {
                                let label = if dev.len() > 40 { format!("{}...", &dev[..37]) } else { dev.clone() };
                                if ui.selectable_value(&mut self.selected_input, dev.clone(), label).changed() {
                                    let _ = self.tx.send(AppMessage::SetInputDevice(dev.clone()));
                                }
                            }
                        });
                });

                ui.add_space(14.0);

                ui.label(egui::RichText::new("MODE").color(DIM).size(11.0));
                ui.scope(|ui| {
                    ui.visuals_mut().selection.bg_fill = egui::Color32::from_rgb(60, 60, 60);
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.mode, Mode::Dtmf, "DTMF");
                        ui.selectable_value(&mut self.mode, Mode::Mf, "MF");
                    });
                });

                ui.add_space(10.0);
                ui.label(egui::RichText::new("DURATION").color(DIM).size(11.0));
                ui.add(egui::Slider::new(&mut self.tone_ms, 40..=600).suffix(" ms"));

                ui.add_space(12.0);
                ui.label(egui::RichText::new("DIAL STRING").color(DIM).size(11.0));
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
                                egui::Button::new(
                                    egui::RichText::new("ABORT").color(WHITE),
                                )
                                    .fill(egui::Color32::from_rgb(60, 14, 14)),
                            )
                            .clicked()
                        {
                            self.stop();
                        }
                    } else if ui
                        .add_sized(
                            [110.0, 28.0],
                            egui::Button::new(
                                egui::RichText::new("DIAL").color(GREEN),
                            )
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
                ui.label(egui::RichText::new("SUPERVISORY").color(DIM).size(11.0));
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
                ui.label(egui::RichText::new("KEYPAD").color(DIM).size(11.0));
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

        egui::CentralPanel::default().frame(terminal_frame).show_inside(ui, |ui| {

            ui.heading(egui::RichText::new("SIGNAL OSCILLOSCOPE").color(GREEN));
            ui.separator();

            let osc_height = 100.0;
            let (response, painter) = ui.allocate_painter(egui::vec2(ui.available_width(), osc_height), egui::Sense::hover());
            let rect = response.rect;

            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(8, 8, 8));
            // FIX: egui::StrokeKind::Inside successfully added
            painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
            painter.line_segment([egui::pos2(rect.left(), rect.center().y), egui::pos2(rect.right(), rect.center().y)], egui::Stroke::new(1.0, egui::Color32::from_rgb(20, 40, 20)));

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
                painter.text(rect.left_top() + egui::vec2(8.0, 8.0), egui::Align2::LEFT_TOP, freq_text, egui::FontId::monospace(14.0), GREEN);
            } else {
                painter.line_segment([egui::pos2(rect.left(), rect.center().y), egui::pos2(rect.right(), rect.center().y)], egui::Stroke::new(2.0, DIM));
                painter.text(rect.left_top() + egui::vec2(8.0, 8.0), egui::Align2::LEFT_TOP, "IDLE", egui::FontId::monospace(14.0), DIM);
            }

            ui.add_space(10.0);

            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("TX/RX LOG").color(GREEN));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("clear").clicked() {
                        self.log_text.clear();
                        self.last_activity_time = None;
                    }
                });
            });
            ui.separator();

            let mut display_text = self.log_text.clone();
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add_sized(
                        ui.available_size(),
                        egui::TextEdit::multiline(&mut display_text)
                            .frame(egui::Frame::none())
                            .font(egui::TextStyle::Monospace)
                            .text_color(GREEN),
                    );
                });
        });

        if self.show_routing {
            let mut close_routing = false;
            egui::Window::new("AUDIO ROUTING")
                .collapsible(false).resizable(false)
                .open(&mut self.show_routing)
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgb(15, 15, 15)))
                .show(&ctx, |ui| {
                    ui.label(egui::RichText::new("DETECTED KERNEL ENDPOINTS").color(GREEN).strong());
                    ui.separator();

                    ui.label(egui::RichText::new("INPUT (RX)").color(DIM));
                    for dev in &self.input_devices {
                        ui.label(format!("- {}", dev));
                    }
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("NOTE: To capture specific applications, route them to a Voicemeeter AUX Virtual Cable in Windows, and set that cable via the Dropdown in the Control Deck.").color(egui::Color32::from_rgb(200, 150, 0)));
                    ui.separator();
                    if ui.button("[ CLOSE ]").clicked() { close_routing = true; }
                });
            if close_routing { self.show_routing = false; }
        }

        if self.show_about {
            let mut acknowledge_clicked = false;

            egui::Window::new("SYS_INFO")
                .collapsible(false)
                .resizable(false)
                .open(&mut self.show_about)
                .frame(egui::Frame::window(&ctx.style()).fill(egui::Color32::from_rgb(15, 15, 15)))
                .show(&ctx, |ui| {
                    ui.heading(egui::RichText::new("PROJECT: ZDL-ECHO").color(egui::Color32::from_rgb(0, 255, 0)));
                    ui.label("TELECOM RESEARCH & SIGNALING TOOLKIT");
                    ui.separator();

                    ui.add_space(6.0);
                    ui.label("DTMF — touch-tone dialing");
                    ui.label("MF    — R1 inter-office (KP / digits / ST)");
                    ui.label("2600  — single-frequency supervisory");
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("space = stop all tones").color(egui::Color32::from_rgb(150, 150, 150)));
                    ui.add_space(10.0);
                    ui.separator();

                    ui.label("Created By: havok");
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("BROUGHT TO YOU BY ZERO DAY LABS")
                        .color(egui::Color32::from_rgb(255, 176, 0))
                        .strong());

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
            Duration::from_millis(50)
        };
        ctx.request_repaint_after(repaint);
    }
}