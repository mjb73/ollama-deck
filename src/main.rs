//! Ollama Deck - a compact desktop widget for loading, unloading and watching
//! Ollama models on a Linux desktop.
//!
//! Usage:
//!   ollama-deck                     open the widget
//!   ollama-deck --screenshot a.png  draw a few frames, save a PNG, exit

mod monitor;
mod ollama;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    Frame, Layout, Margin, RichText, Sense, Stroke, StrokeKind, Vec2,
};

use monitor::{Sample, Tick};
use ollama::{LoadedModel, ModelInfo, ShowInfo};

// ---------------------------------------------------------------- palette

const BG_TOP: Color32 = Color32::from_rgb(13, 20, 27);
const BG_BOT: Color32 = Color32::from_rgb(8, 12, 17);
const CARD: Color32 = Color32::from_rgb(18, 26, 35);
const CARD_HI: Color32 = Color32::from_rgb(27, 38, 49);
const TRACK: Color32 = Color32::from_rgb(11, 17, 23);
const LINE: Color32 = Color32::from_rgb(37, 51, 64);
const TXT: Color32 = Color32::from_rgb(226, 236, 244);
const DIM: Color32 = Color32::from_rgb(139, 158, 176);
const FAINT: Color32 = Color32::from_rgb(96, 116, 134);
const ACCENT: Color32 = Color32::from_rgb(98, 226, 200);
const ACCENT_SOFT: Color32 = Color32::from_rgb(22, 58, 54);
const OK: Color32 = Color32::from_rgb(110, 231, 183);
const WARN: Color32 = Color32::from_rgb(251, 191, 96);
const BAD: Color32 = Color32::from_rgb(248, 113, 113);
const BLUE: Color32 = Color32::from_rgb(125, 178, 245);
const VIOLET: Color32 = Color32::from_rgb(178, 158, 245);

const HISTORY: usize = 72;
const SHOT_FRAME: u64 = 120;

/// Set once a `--screenshot` capture has been written, so a run that never
/// painted a picture can report it instead of exiting quietly.
static SHOT_SAVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const KEEP_ALIVE: [i64; 4] = [5, 15, 60, -1];
const WIN_W: f32 = 396.0;
const WIN_H: f32 = 646.0;

// ---------------------------------------------------------------- messages

enum Msg {
    Tick(Tick),
    Models(Result<Vec<ModelInfo>, String>),
    Show(String, Result<ShowInfo, String>),
    Op { ok: bool, text: String },
}

#[derive(Clone, Copy, PartialEq)]
enum OpKind {
    Load,
    Unload,
}

// ---------------------------------------------------------------- app

struct Deck {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    sample: Sample,
    cpu_hist: VecDeque<f32>,
    ram_hist: VecDeque<f32>,
    vram_hist: VecDeque<f32>,

    online: bool,
    version: String,
    loaded: Vec<LoadedModel>,

    models: Vec<ModelInfo>,
    selected: String,
    models_waiting: bool,
    last_models: Instant,

    ctx: u32,
    show_for: String,
    show: Option<ShowInfo>,
    show_waiting: bool,

    keep_idx: usize,
    busy: Option<(OpKind, String)>,
    toast: Option<(bool, String, Instant)>,

    on_top: bool,
    level_sent: bool,
    user_picked: bool,

    shot_path: Option<PathBuf>,
    quiet: bool,
    frames: u64,
    samples: u32,
    started: Instant,
    shot_fired: bool,
    shot_at: Option<Instant>,
}

impl Deck {
    fn new(cc: &eframe::CreationContext<'_>, shot_path: Option<PathBuf>) -> Self {
        install_look(&cc.egui_ctx);
        let (tx, rx) = channel();

        let mut app = Self {
            tx,
            rx,
            sample: Sample::default(),
            cpu_hist: VecDeque::new(),
            ram_hist: VecDeque::new(),
            vram_hist: VecDeque::new(),
            online: false,
            version: String::new(),
            loaded: Vec::new(),
            models: Vec::new(),
            selected: String::new(),
            models_waiting: false,
            last_models: Instant::now() - Duration::from_secs(600),
            ctx: 8192,
            show_for: String::new(),
            show: None,
            show_waiting: false,
            keep_idx: 0,
            busy: None,
            toast: None,
            on_top: true,
            level_sent: false,
            user_picked: false,
            quiet: shot_path.is_some(),
            shot_path,
            frames: 0,
            samples: 0,
            started: Instant::now(),
            shot_fired: false,
            shot_at: None,
        };
        app.refresh_models();
        monitor::spawn(app.tx.clone());
        app.arm_shot_watchdog();
        if app.shot_path.is_some() {
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                for _ in 0..400 {
                    ctx.request_repaint();
                    std::thread::sleep(Duration::from_millis(50));
                }
            });
        }
        app
    }

    fn arm_shot_watchdog(&self) {
        if self.shot_path.is_none() {
            return;
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(60));
            eprintln!("ollama-deck: screenshot timed out");
            std::process::exit(4);
        });
    }

    // ------------------------------------------------------------ network

    fn refresh_models(&mut self) {
        if self.models_waiting || self.busy.is_some() {
            return;
        }
        self.models_waiting = true;
        self.last_models = Instant::now();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Models(ollama::list_models()));
        });
    }

    fn request_show(&mut self, model: String) {
        if model.is_empty() || (self.show_waiting && self.show_for == model) {
            return;
        }
        self.show_waiting = true;
        self.show_for = model.clone();
        self.show = None;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = ollama::show(&model);
            let _ = tx.send(Msg::Show(model, result));
        });
    }

    fn start_load(&mut self) {
        if self.selected.is_empty() || self.busy.is_some() {
            return;
        }
        let model = self.selected.clone();
        let ctx = self.ctx;
        let keep = KEEP_ALIVE[self.keep_idx.min(KEEP_ALIVE.len() - 1)];
        let cloud = self
            .models
            .iter()
            .find(|m| m.name == model)
            .map(|m| m.cloud)
            .unwrap_or(false);
        // A resident model keeps its old context unless the runner is rebuilt,
        // so a changed size means dropping the model first.
        let reload = self
            .loaded
            .iter()
            .find(|l| ollama::same_model(&l.name, &model))
            .map(|l| l.ctx == 0 || l.ctx != ctx)
            .unwrap_or(false);
        self.busy = Some((OpKind::Load, model.clone()));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if reload {
                if let Err(e) = ollama::unload(&model) {
                    let _ = tx.send(Msg::Op { ok: false, text: e });
                    return;
                }
            }
            let (ok, text) = match ollama::load(&model, ctx, keep) {
                Ok(report) => load_message(&model, ctx, cloud, report),
                Err(e) => (false, e),
            };
            let _ = tx.send(Msg::Op { ok, text });
        });
    }

    fn start_unload(&mut self, model: String) {
        if self.busy.is_some() || model.is_empty() {
            return;
        }
        self.busy = Some((OpKind::Unload, model.clone()));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let (ok, text) = match ollama::unload(&model) {
                Ok(()) => (true, format!("{} unloaded", short_name(&model))),
                Err(e) => (false, e),
            };
            let _ = tx.send(Msg::Op { ok, text });
        });
    }

    // ------------------------------------------------------------ plumbing

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Tick(tick) => self.push_tick(tick),
            Msg::Models(result) => {
                self.models_waiting = false;
                match result {
                    Ok(models) => {
                        self.models = models;
                        let still_there = self.models.iter().any(|m| m.name == self.selected);
                        if self.selected.is_empty() || !still_there {
                            let next = self
                                .models
                                .iter()
                                .find(|m| self.loaded.iter().any(|l| l.name == m.name))
                                .or_else(|| self.models.iter().find(|m| !m.cloud))
                                .or_else(|| self.models.first())
                                .map(|m| m.name.clone())
                                .unwrap_or_default();
                            self.set_model(next);
                        }
                    }
                    Err(e) => {
                        if self.models.is_empty() {
                            self.toast = Some((false, e, Instant::now()));
                        }
                    }
                }
            }
            Msg::Show(model, result) => {
                if model == self.show_for {
                    self.show_waiting = false;
                    self.show = result.ok();
                }
            }
            Msg::Op { ok, text } => {
                self.busy = None;
                self.toast = Some((ok, text, Instant::now()));
                self.models_waiting = false;
                self.refresh_models();
            }
        }
    }

    fn push_tick(&mut self, tick: Tick) {
        let s = tick.sample;
        push_hist(&mut self.cpu_hist, s.cpu_pct.clamp(0.0, 100.0) / 100.0);
        let ram_frac = if s.ram_total > 0 {
            s.ram_used as f32 / s.ram_total as f32
        } else {
            0.0
        };
        push_hist(&mut self.ram_hist, ram_frac);
        let vram_frac = if s.vram_total > 0 {
            s.vram_used as f32 / s.vram_total as f32
        } else {
            0.0
        };
        push_hist(&mut self.vram_hist, vram_frac);

        self.sample = s;
        self.samples = self.samples.saturating_add(1);
        self.online = tick.online;
        self.version = tick.version;
        self.loaded = tick.loaded;

        if !self.user_picked {
            if let Some(first) = self.loaded.first() {
                if first.name != self.selected {
                    self.user_picked = true;
                    let name = first.name.clone();
                    self.set_model(name);
                }
            }
        }

        let stale = self.last_models.elapsed() > Duration::from_secs(45);
        if (!self.online && self.models.is_empty()) || (self.online && stale) {
            self.refresh_models();
        }
    }

    fn set_model(&mut self, name: String) {
        if name == self.selected {
            if !name.is_empty() && self.show.is_none() && !self.show_waiting {
                self.request_show(name);
            }
            return;
        }
        self.selected = name.clone();
        self.show = None;
        self.show_for.clear();
        self.show_waiting = false;
        if !name.is_empty() {
            let max = self
                .models
                .iter()
                .find(|m| m.name == name)
                .map(|m| m.ctx_max)
                .unwrap_or(0);
            if max > 0 {
                self.ctx = self.ctx.min(round_ctx(max));
            }
            self.request_show(name);
        }
    }

    fn ctx_max(&self) -> u32 {
        let from_show = self.show.as_ref().map(|s| s.ctx_max).unwrap_or(0);
        let from_list = self
            .models
            .iter()
            .find(|m| m.name == self.selected)
            .map(|m| m.ctx_max)
            .unwrap_or(0);
        let max = from_show.max(from_list);
        if max == 0 {
            32768
        } else {
            round_ctx(max)
        }
    }

    fn weights(&self) -> u64 {
        self.models
            .iter()
            .find(|m| m.name == self.selected)
            .map(|m| m.size)
            .unwrap_or(0)
    }

    fn estimate(&self) -> Option<(u64, u64, u64)> {
        let show = self.show.as_ref()?;
        let kv = show.kv_bytes(self.ctx);
        if kv == 0 {
            return None;
        }
        let weights = self.weights();
        Some((weights, kv, weights + kv))
    }

    fn is_loaded(&self, name: &str) -> bool {
        self.loaded
            .iter()
            .any(|l| ollama::same_model(&l.name, name))
    }

    // ------------------------------------------------------------ drawing

    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 7.0;
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(9.0), Sense::hover());
            let color = if self.online { OK } else { BAD };
            let c = dot.center();
            let glow = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 40);
            ui.painter().circle_filled(c, 7.0, glow);
            ui.painter().circle_filled(c, 3.5, color);
            ui.label(
                RichText::new("OLLAMA DECK")
                    .font(FontId::proportional(12.5))
                    .color(TXT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                if chip(ui, "TOP", self.on_top).clicked() {
                    self.on_top = !self.on_top;
                    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if self.on_top {
                        egui::WindowLevel::AlwaysOnTop
                    } else {
                        egui::WindowLevel::Normal
                    }));
                }
                let refresh_tip = if self.quiet {
                    ""
                } else {
                    "Reload the model list"
                };
                if icon_button(ui, Icon::Refresh, false, refresh_tip).clicked() {
                    self.refresh_models();
                }
                if !self.version.is_empty() {
                    ui.label(
                        RichText::new(format!("v{}", self.version))
                            .font(FontId::monospace(9.0))
                            .color(FAINT),
                    );
                }
            });
        });
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(ollama::pretty_endpoint())
                    .font(FontId::monospace(9.0))
                    .color(FAINT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, color) = if self.online {
                    ("online".to_string(), OK)
                } else {
                    ("offline".to_string(), BAD)
                };
                pill(ui, &text, color);
            });
        });
    }

    fn model_card(&mut self, ui: &mut egui::Ui) {
        let loaded_selected = self.is_loaded(&self.selected);
        let resident_ctx = self
            .loaded
            .iter()
            .find(|l| ollama::same_model(&l.name, &self.selected))
            .map(|l| l.ctx)
            .unwrap_or(0);
        // Ollama only rebuilds its runner (and therefore its context window)
        // when the model is dropped first; asking for a new size while it is
        // resident silently keeps the old one.
        let ctx_changed = loaded_selected && (resident_ctx == 0 || resident_ctx != self.ctx);

        ui.horizontal(|ui| {
            section_label(ui, "MODEL");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let detail = self
                    .models
                    .iter()
                    .find(|m| m.name == self.selected)
                    .map(|m| {
                        let mut s = fmt_bytes(m.size);
                        if !m.quant.is_empty() {
                            s.push_str(" · ");
                            s.push_str(&m.quant);
                        }
                        if !m.params.is_empty() {
                            s.push_str(" · ");
                            s.push_str(&m.params);
                        }
                        s
                    })
                    .unwrap_or_default();
                if !detail.is_empty() {
                    tiny(ui, &detail, FAINT);
                }
            });
        });

        let width = ui.available_width();
        let selected_text = if self.selected.is_empty() {
            if self.online {
                "no models installed".to_string()
            } else {
                "waiting for Ollama".to_string()
            }
        } else {
            short_name(&self.selected)
        };
        let mut new_model: Option<String> = None;
        let enabled = !self.models.is_empty() && self.busy.is_none();
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt("model-picker")
                .selected_text(RichText::new(selected_text).font(FontId::monospace(12.0)))
                .width(width - 4.0)
                .height(280.0)
                .show_ui(ui, |ui| {
                    ui.set_min_width(width - 22.0);
                    for m in &self.models {
                        let mut text = format!("{:<26}", short_name(&m.name));
                        text.push_str(&fmt_bytes(m.size));
                        let mut rich = RichText::new(text).font(FontId::monospace(10.5));
                        if self.is_loaded(&m.name) {
                            rich = rich.color(OK);
                        }
                        if ui.selectable_label(m.name == self.selected, rich).clicked() {
                            new_model = Some(m.name.clone());
                        }
                    }
                });
        });
        if let Some(name) = new_model {
            self.user_picked = true;
            self.set_model(name);
        }

        ui.add_space(5.0);
        ui.horizontal(|ui| {
            section_label(ui, "CONTEXT");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let max = self.ctx_max();
                tiny(ui, &format!("max {}", fmt_ctx(max)), FAINT);
            });
        });

        let max = self.ctx_max();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
            for step in [2048u32, 4096, 8192, 16384, 32768, 65536, 131072] {
                if step > max + 2047 {
                    continue;
                }
                if chip(ui, &fmt_ctx(step), self.ctx == step).clicked() {
                    self.ctx = step;
                }
            }
        });

        ui.add_space(3.0);
        let mut ctx = self.ctx;
        let slider = ui.add(
            egui::Slider::new(&mut ctx, 1024..=max)
                .step_by(1024.0)
                .trailing_fill(true)
                .custom_formatter(|n, _| fmt_ctx(n as u32)),
        );
        if slider.changed() {
            self.ctx = round_ctx(ctx.max(1024));
        }

        ui.add_space(3.0);
        ui.horizontal(|ui| {
            let text = if ctx_changed {
                let loaded_at = if resident_ctx > 0 {
                    fmt_ctx(resident_ctx)
                } else {
                    "an unknown size".to_string()
                };
                format!(
                    "loaded at {} · press Reload to apply {}",
                    loaded_at,
                    fmt_ctx(self.ctx)
                )
            } else {
                match self.estimate() {
                    Some((_, kv, total)) => {
                        if self.sample.vram_total > 0 {
                            format!(
                                "≈ {} needed · {} cache · {} free",
                                fmt_bytes(total),
                                fmt_bytes(kv),
                                fmt_bytes(self.sample.vram_free())
                            )
                        } else {
                            format!("≈ {} needed · {} cache", fmt_bytes(total), fmt_bytes(kv))
                        }
                    }
                    None => "≈ weights only until the model reports its size".to_string(),
                }
            };
            let selected_vram: u64 = self
                .loaded
                .iter()
                .filter(|l| ollama::same_model(&l.name, &self.selected))
                .map(|l| l.size_vram)
                .sum();
            // Reloading this model frees its own VRAM first, so the budget is
            // what is free right now plus whatever this model already holds.
            let available = self.sample.vram_free().saturating_add(selected_vram);
            let fits = match self.estimate() {
                Some((_, _, total)) if self.sample.vram_total > 0 => total <= available,
                _ => true,
            };
            tiny(ui, &text, if fits && !ctx_changed { FAINT } else { WARN });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.sample.vram_total > 0 && self.estimate().is_some() {
                    if fits {
                        pill(ui, "fits", OK);
                    } else {
                        pill(ui, "too big", BAD);
                    }
                }
            });
        });

        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let busy_here = self
                .busy
                .as_ref()
                .map(|(kind, model)| *kind == OpKind::Load && *model == self.selected)
                .unwrap_or(false);
            let load_label = if busy_here {
                "Loading…"
            } else if ctx_changed {
                "Reload"
            } else if loaded_selected {
                "Loaded"
            } else {
                "Load"
            };
            let can_load = self.online
                && !self.selected.is_empty()
                && (!loaded_selected || ctx_changed)
                && self.busy.is_none();
            let w = (ui.available_width() - 78.0) * 0.5;
            let load_btn = primary_button(ui, load_label, can_load, w.max(70.0));
            let load_tip = if ctx_changed {
                "Unload this model and load it again so the new context size takes effect"
            } else {
                "Load this model into memory with the chosen context size"
            };
            if tip(load_btn, load_tip, self.quiet).clicked() {
                self.start_load();
            }
            let can_unload = self.online && loaded_selected && self.busy.is_none();
            let unload_btn = ghost_button(ui, "Unload", can_unload, w.max(70.0));
            if tip(
                unload_btn,
                "Free this model's memory straight away",
                self.quiet,
            )
            .clicked()
            {
                self.start_unload(self.selected.clone());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let keep = KEEP_ALIVE[self.keep_idx.min(KEEP_ALIVE.len() - 1)];
                let label = if keep < 0 {
                    "keep ∞".to_string()
                } else if keep < 60 {
                    format!("keep {keep}m")
                } else {
                    format!("keep {}h", keep / 60)
                };
                let keep_btn = ghost_button(ui, &label, true, 74.0);
                if tip(
                    keep_btn,
                    "How long the model stays in memory after you stop using it",
                    self.quiet,
                )
                .clicked()
                {
                    self.keep_idx = (self.keep_idx + 1) % KEEP_ALIVE.len();
                }
            });
        });

        if !self.online {
            ui.add_space(5.0);
            ui.label(
                RichText::new("Ollama is not answering — run “ollama serve”, then reload.")
                    .font(FontId::proportional(10.0))
                    .color(WARN),
            );
        }
    }

    fn loaded_card(&mut self, ui: &mut egui::Ui) {
        let mut unload: Option<String> = None;
        ui.horizontal(|ui| {
            section_label(ui, "LOADED");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let used: u64 = self.loaded.iter().map(|l| l.size_vram).sum();
                let text = if used > 0 {
                    format!("{} in VRAM", fmt_bytes(used))
                } else {
                    "nothing resident".to_string()
                };
                tiny(ui, &text, FAINT);
            });
        });
        ui.add_space(3.0);
        if self.loaded.is_empty() {
            ui.label(
                RichText::new(if self.online {
                    "no model in memory — pick one above and press Load"
                } else {
                    "no server to ask"
                })
                .font(FontId::proportional(10.5))
                .color(FAINT),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("loaded-list")
            .max_height(94.0)
            .auto_shrink([false, true])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                for item in &self.loaded {
                    let (rect, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::hover());
                    let bg = if resp.hovered() { CARD_HI } else { TRACK };
                    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);

                    let full = item.size.max(1) as f32;
                    let in_vram = (item.size_vram as f32 / full).clamp(0.0, 1.0);
                    let bar = egui::Rect::from_min_size(
                        rect.left_top() + Vec2::new(1.0, 5.0),
                        Vec2::new(2.0, (rect.height() - 10.0) * in_vram.max(0.12)),
                    );
                    ui.painter().rect_filled(bar, CornerRadius::same(1), ACCENT);

                    ui.painter().text(
                        rect.left_top() + Vec2::new(11.0, 7.0),
                        Align2::LEFT_TOP,
                        short_name(&item.name),
                        FontId::monospace(11.0),
                        TXT,
                    );
                    let ctx_text = if item.ctx > 0 {
                        fmt_ctx(item.ctx)
                    } else {
                        "?".to_string()
                    };
                    let host_ram = item.size.saturating_sub(item.size_vram);
                    let mut sub = format!("{ctx_text} ctx  ·  {} vram", fmt_bytes(item.size_vram));
                    if host_ram > 0 {
                        sub.push_str(&format!("  ·  {} ram", fmt_bytes(host_ram)));
                    }
                    if let Some(left) = expiry_left(&item.expires_at) {
                        sub = format!("{sub}  ·  {left}");
                    }
                    ui.painter().text(
                        rect.left_bottom() + Vec2::new(11.0, -7.0),
                        Align2::LEFT_BOTTOM,
                        sub,
                        FontId::monospace(8.5),
                        FAINT,
                    );

                    let kill = egui::Rect::from_center_size(
                        egui::pos2(rect.right() - 15.0, rect.center().y),
                        Vec2::splat(24.0),
                    );
                    let btn = ui.interact(
                        kill,
                        ui.scope_id().with(("unload", &item.name)),
                        Sense::click(),
                    );
                    let color = if btn.hovered() { BAD } else { FAINT };
                    if btn.hovered() {
                        ui.painter()
                            .rect_filled(kill, CornerRadius::same(4), CARD_HI);
                    }
                    draw_icon(ui.painter(), kill.center(), 8.0, Icon::Cross, color, 1.2);
                    if btn.clicked() {
                        unload = Some(item.name.clone());
                    }
                }
            });
        if let Some(name) = unload {
            self.start_unload(name);
        }
    }

    fn monitor_card(&mut self, ui: &mut egui::Ui) {
        let s = &self.sample;
        let vram_frac = if s.vram_total > 0 {
            Some(s.vram_used as f32 / s.vram_total as f32)
        } else {
            None
        };
        let ram_frac = if s.ram_total > 0 {
            Some(s.ram_used as f32 / s.ram_total as f32)
        } else {
            None
        };
        let cpu_val = format!("{:.0}%", s.cpu_pct);
        let ram_val = if s.ram_total > 0 {
            format!("{} / {}", fmt_bytes(s.ram_used), fmt_bytes(s.ram_total))
        } else {
            "n/a".to_string()
        };
        let vram_val = if s.vram_total > 0 {
            format!("{} / {}", fmt_bytes(s.vram_used), fmt_bytes(s.vram_total))
        } else {
            "no GPU".to_string()
        };

        ui.horizontal(|ui| {
            section_label(ui, "LIVE");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let note = if let Some(pid) = s.ollama_pid {
                    format!(
                        "ollama {:.0}% · {} · pid {pid}",
                        s.ollama_cpu,
                        fmt_bytes(s.ollama_rss)
                    )
                } else {
                    "ollama process idle".to_string()
                };
                tiny(ui, &note, FAINT);
            });
        });

        let gpu_line = if s.vram_total > 0 { 18.0 } else { 0.0 };
        let per_meter = ((ui.available_height() - gpu_line) / 3.0).max(34.0);
        let spark_h = (per_meter - 30.0).clamp(8.0, 58.0);

        meter(
            ui,
            "CPU",
            &cpu_val,
            s.cpu_pct / 100.0,
            None,
            &self.cpu_hist,
            ACCENT,
            spark_h,
        );
        meter(
            ui,
            "RAM",
            &ram_val,
            ram_frac.unwrap_or(0.0),
            ram_frac,
            &self.ram_hist,
            BLUE,
            spark_h,
        );
        meter(
            ui,
            "VRAM",
            &vram_val,
            vram_frac.unwrap_or(0.0),
            vram_frac,
            &self.vram_hist,
            VIOLET,
            spark_h,
        );

        if self.sample.vram_total > 0 {
            ui.add_space(3.0);
            let gpu = self
                .sample
                .gpu_util
                .map(|u| format!("gpu load {u:.0}%"))
                .unwrap_or_else(|| "gpu load n/a".to_string());
            let name = if self.sample.gpu_name.is_empty() {
                "gpu".to_string()
            } else {
                self.sample.gpu_name.clone()
            };
            let line = ui.label(
                RichText::new(format!(
                    "{name} · {} free · {gpu}",
                    fmt_bytes(self.sample.vram_free())
                ))
                .font(FontId::monospace(8.5))
                .color(FAINT),
            );
            let source = if self.sample.gpu_source.is_empty() {
                "the graphics driver"
            } else {
                &self.sample.gpu_source
            };
            tip(
                line,
                &format!("memory numbers come from {source}"),
                self.quiet,
            );
        }
    }

    fn toast(&mut self, ui: &mut egui::Ui) {
        let Some((ok, text, at)) = self.toast.clone() else {
            return;
        };
        if at.elapsed() > Duration::from_secs(12) {
            self.toast = None;
            return;
        }
        let color = if ok { OK } else { BAD };
        let bg = if ok {
            Color32::from_rgba_unmultiplied(22, 60, 48, 220)
        } else {
            Color32::from_rgba_unmultiplied(64, 25, 30, 220)
        };
        Frame::new()
            .fill(bg)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(8, 5))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(truncate(&text, 76))
                        .font(FontId::monospace(9.5))
                        .color(color),
                );
            });
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let note = if self.sample.gpu_source.is_empty() {
                "RAM / VRAM / CPU · 1 s samples".to_string()
            } else {
                format!("CPU · RAM · VRAM · 1 s · {}", self.sample.gpu_source)
            };
            ui.label(
                RichText::new(note)
                    .font(FontId::monospace(8.5))
                    .color(FAINT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (text, color) = if self.online {
                    ("live", OK)
                } else {
                    ("paused", FAINT)
                };
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(7.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 2.5, color);
                ui.add_space(3.0);
                ui.label(
                    RichText::new(text)
                        .font(FontId::monospace(8.5))
                        .color(color),
                );
            });
        });
    }
}

impl eframe::App for Deck {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BG_BOT.to_normalized_gamma_f32()
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;
        self.frames += 1;
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }

        if !self.level_sent {
            self.level_sent = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if self.on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
            if self.shot_path.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }

        if let Some(path) = self.shot_path.clone() {
            ctx.request_repaint();
            let waited = self.started.elapsed();
            // Aim for a fixed number of frames, but never hang on them: a
            // compositor can stop handing out frames to a window that ends up
            // behind another one, so a wall-clock deadline takes over.
            let settled = (self.frames >= SHOT_FRAME
                && (self.samples >= 6 || waited > Duration::from_secs(9)))
                || (self.frames >= 4 && waited > Duration::from_secs(4));
            if settled && !self.shot_fired {
                self.shot_fired = true;
                self.shot_at = Some(Instant::now());
                let ctx2 = ctx.clone();
                ctx.request_screenshot(move |image| {
                    match save_png(&path, &image) {
                        Ok(()) => {
                            SHOT_SAVED.store(true, std::sync::atomic::Ordering::SeqCst);
                            eprintln!("ollama-deck: saved {}", path.display());
                        }
                        Err(e) => eprintln!("ollama-deck: screenshot failed: {e}"),
                    }
                    ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
                });
            }
            if let Some(at) = self.shot_at {
                if self.frames > SHOT_FRAME + 240 || at.elapsed() > Duration::from_secs(3) {
                    std::process::exit(0);
                }
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        egui::Panel::bottom("status")
            .frame(
                Frame::new()
                    .fill(Color32::from_rgb(10, 15, 21))
                    .inner_margin(Margin::symmetric(12, 3)),
            )
            .show(root, |ui| self.footer(ui));

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(BG_BOT)
                    .inner_margin(Margin::symmetric(12, 9)),
            )
            .show(root, |ui| {
                paint_backdrop(ui);
                self.header(ui, ctx);
                ui.add_space(6.0);
                card(ui, |ui| self.model_card(ui));
                ui.add_space(5.0);
                card(ui, |ui| self.loaded_card(ui));
                ui.add_space(5.0);
                card(ui, |ui| self.monitor_card(ui));
                ui.add_space(3.0);
                self.toast(ui);
            });
    }
}

// ---------------------------------------------------------------- painting helpers

fn push_hist(hist: &mut VecDeque<f32>, value: f32) {
    hist.push_back(value.clamp(0.0, 1.0));
    while hist.len() > HISTORY {
        hist.pop_front();
    }
}

fn paint_backdrop(ui: &egui::Ui) {
    let rect = ui.max_rect().expand(30.0);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), BG_TOP);
    mesh.colored_vertex(rect.right_top(), Color32::from_rgb(11, 18, 25));
    mesh.colored_vertex(rect.right_bottom(), BG_BOT);
    mesh.colored_vertex(rect.left_bottom(), BG_BOT);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(10, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(FontId::monospace(8.5))
            .color(FAINT),
    );
}

fn tip(resp: egui::Response, text: &str, quiet: bool) -> egui::Response {
    if quiet {
        resp
    } else {
        resp.on_hover_text(text)
    }
}

fn tiny(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.label(
        RichText::new(text)
            .font(FontId::monospace(8.5))
            .color(color),
    );
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let font = FontId::monospace(8.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let size = Vec2::new(galley.size().x + 13.0, 15.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let bg = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 24);
    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(7),
        Stroke::new(1.0, color.gamma_multiply(0.45)),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
}

fn chip(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let font = FontId::monospace(9.5);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, TXT);
    let size = Vec2::new(galley.size().x + 13.0, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let (bg, stroke, fg) = if selected {
        (ACCENT_SOFT, Stroke::new(1.0, ACCENT), ACCENT)
    } else if resp.hovered() {
        (CARD_HI, Stroke::new(1.0, LINE), TXT)
    } else {
        (TRACK, Stroke::new(1.0, LINE), DIM)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(5), stroke, StrokeKind::Inside);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool, width: f32) -> egui::Response {
    let font = FontId::monospace(11.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 27.0), Sense::click());
    let hovered = enabled && resp.hovered();
    let bg = if !enabled {
        Color32::from_rgb(23, 31, 39)
    } else if hovered {
        Color32::from_rgb(126, 242, 218)
    } else {
        ACCENT
    };
    let fg = if enabled {
        Color32::from_rgb(7, 20, 18)
    } else {
        FAINT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

fn ghost_button(ui: &mut egui::Ui, label: &str, enabled: bool, width: f32) -> egui::Response {
    let font = FontId::monospace(10.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, DIM);
    let w = if width > 0.0 {
        width
    } else {
        galley.size().x + 16.0
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 27.0), Sense::click());
    let (bg, stroke, fg) = if !enabled {
        (
            Color32::TRANSPARENT,
            Stroke::new(1.0, LINE.gamma_multiply(0.55)),
            FAINT,
        )
    } else if resp.hovered() {
        (CARD_HI, Stroke::new(1.0, ACCENT.gamma_multiply(0.5)), TXT)
    } else {
        (Color32::TRANSPARENT, Stroke::new(1.0, LINE), DIM)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter()
        .rect_stroke(rect, CornerRadius::same(6), stroke, StrokeKind::Inside);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

#[derive(Clone, Copy)]
enum Icon {
    Refresh,
    Cross,
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, on: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    let bg = if on {
        ACCENT_SOFT
    } else if resp.hovered() {
        CARD_HI
    } else {
        Color32::TRANSPARENT
    };
    if on || resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(5), bg);
    }
    let color = if on {
        ACCENT
    } else if resp.hovered() {
        TXT
    } else {
        DIM
    };
    draw_icon(ui.painter(), rect.center(), 8.0, icon, color, 1.3);
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

fn draw_icon(painter: &egui::Painter, c: egui::Pos2, r: f32, icon: Icon, color: Color32, w: f32) {
    let stroke = Stroke::new(w, color);
    match icon {
        Icon::Refresh => {
            // open ring with a small arrow head, reading as "reload"
            let a0 = -1.25f32;
            let a1 = 4.35f32;
            let mut points = Vec::new();
            for i in 0..=28 {
                let t = i as f32 / 28.0;
                let a = a0 + (a1 - a0) * t;
                points.push(egui::pos2(
                    c.x + a.cos() * r * 0.66,
                    c.y + a.sin() * r * 0.66,
                ));
            }
            let end = *points.last().unwrap();
            painter.add(egui::Shape::line(points, stroke));
            let tan = egui::vec2(-a1.sin(), a1.cos());
            let nrm = egui::vec2(-tan.y, tan.x);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    end + tan * 3.6,
                    end + nrm * 3.0 - tan * 1.2,
                    end - nrm * 3.0 - tan * 1.2,
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Cross => {
            painter.line_segment(
                [
                    egui::pos2(c.x - r * 0.42, c.y - r * 0.42),
                    egui::pos2(c.x + r * 0.42, c.y + r * 0.42),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(c.x + r * 0.42, c.y - r * 0.42),
                    egui::pos2(c.x - r * 0.42, c.y + r * 0.42),
                ],
                stroke,
            );
        }
    }
}

fn meter(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    level: f32,
    frac: Option<f32>,
    hist: &VecDeque<f32>,
    color: Color32,
    height: f32,
) {
    ui.add_space(3.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).font(FontId::monospace(9.0)).color(DIM));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let pct = frac
                .map(|f| format!("  {:.0}%", f * 100.0))
                .unwrap_or_default();
            ui.label(
                RichText::new(format!("{value}{pct}"))
                    .font(FontId::monospace(10.5))
                    .color(if level > 0.92 { WARN } else { TXT }),
            );
        });
    });
    ui.add_space(1.0);
    let h = height;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), TRACK);
    for i in 1..4 {
        let x = rect.left() + rect.width() * i as f32 / 4.0;
        p.line_segment(
            [
                egui::pos2(x, rect.top() + 3.0),
                egui::pos2(x, rect.bottom() - 3.0),
            ],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(74, 94, 112, 34)),
        );
    }
    if hist.len() >= 2 {
        let n = hist.len();
        let step = rect.width() / (n.max(2) - 1) as f32;
        let x0 = rect.left();
        let mut prev: Option<egui::Pos2> = None;
        let mut line = Vec::with_capacity(n);
        let usable = (h - 7.0).max(4.0);
        for (i, v) in hist.iter().enumerate() {
            let x = x0 + i as f32 * step;
            let y = rect.bottom() - 3.5 - v.clamp(0.0, 1.0) * usable;
            let pt = egui::pos2(x, y);
            line.push(pt);
            if let Some(pr) = prev {
                p.add(egui::Shape::convex_polygon(
                    vec![
                        pr,
                        pt,
                        egui::pos2(pt.x, rect.bottom() - 1.0),
                        egui::pos2(pr.x, rect.bottom() - 1.0),
                    ],
                    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 30),
                    Stroke::NONE,
                ));
            }
            prev = Some(pt);
        }
        p.add(egui::Shape::line(line.clone(), Stroke::new(1.4, color)));
        if let Some(last) = line.last() {
            p.circle_filled(*last, 2.0, color);
        }
    }
    if let Some(f) = frac {
        let w = (rect.width() * f.clamp(0.0, 1.0)).max(1.0);
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.bottom() - 2.0),
            Vec2::new(w, 2.0),
        );
        p.rect_filled(bar, CornerRadius::same(1), color.gamma_multiply(0.85));
    }
    let _ = resp;
}

// ---------------------------------------------------------------- formatting

fn short_name(name: &str) -> String {
    name.trim_end_matches(":latest").to_string()
}

/// Turn what Ollama really did into one line the person can act on.
fn load_message(
    model: &str,
    wanted: u32,
    cloud: bool,
    report: ollama::LoadReport,
) -> (bool, String) {
    let short = short_name(model);
    if !report.resident {
        if cloud {
            return (
                true,
                format!("{short} runs in the cloud · nothing on this PC"),
            );
        }
        return (
            false,
            format!(
                "{short} did not stay in memory at {} · Ollama ran out of room",
                fmt_ctx(wanted)
            ),
        );
    }
    let mut text = format!("{short} ready · {} context", fmt_ctx(report.ctx));
    if report.ctx > 0 && report.ctx < wanted {
        text.push_str(&format!(" (capped from {})", fmt_ctx(wanted)));
    }
    let on_cpu = report.on_cpu();
    if report.size > 0 && on_cpu > report.size / 20 {
        text.push_str(&format!(" · {} in RAM", fmt_bytes(on_cpu)));
    }
    (true, text)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn fmt_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "-".to_string();
    }
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        let mb = bytes as f64 / 1_048_576.0;
        format!("{mb:.0} MB")
    }
}

fn fmt_ctx(ctx: u32) -> String {
    if ctx >= 1024 {
        let k = ctx as f32 / 1024.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{}k", k.round() as u32)
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{ctx}")
    }
}

fn round_ctx(ctx: u32) -> u32 {
    if ctx <= 2048 {
        return ctx.max(1024);
    }
    let step = if ctx > 32768 { 8192 } else { 1024 };
    ((ctx + step / 2) / step) * step
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn parse_epoch(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| text.get(a..b)?.parse::<i64>().ok();
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let minute = num(14, 16)?;
    let second = num(17, 19)?;
    Some(days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second)
}

fn expiry_left(expires: &str) -> Option<String> {
    let at = parse_epoch(expires)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let left = at - now;
    if left <= 0 {
        return Some("expiring".to_string());
    }
    Some(if left >= 3600 {
        format!("{}h{}m left", left / 3600, (left % 3600) / 60)
    } else if left >= 60 {
        format!("{}m left", left / 60)
    } else {
        format!("{left}s left")
    })
}

// ---------------------------------------------------------------- look

fn install_look(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let bundled = [
        (
            "fira",
            &include_bytes!("../assets/FiraSans-Regular.ttf")[..],
            FontFamily::Proportional,
        ),
        (
            "hack",
            &include_bytes!("../assets/Hack-Regular.ttf")[..],
            FontFamily::Monospace,
        ),
    ];
    for (name, bytes, family) in bundled {
        fonts
            .font_data
            .insert(name.to_string(), Arc::new(FontData::from_static(bytes)));
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.to_string());
    }
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(6.0, 5.0);
        style.spacing.button_padding = Vec2::new(8.0, 4.0);
        style.spacing.slider_width = 210.0;
        style.spacing.indent = 12.0;
        style.animation_time = 0.08;

        use FontFamily::{Monospace, Proportional};
        style.text_styles = [
            (egui::TextStyle::Small, FontId::new(9.5, Proportional)),
            (egui::TextStyle::Body, FontId::new(11.5, Proportional)),
            (egui::TextStyle::Button, FontId::new(11.5, Proportional)),
            (egui::TextStyle::Heading, FontId::new(14.0, Proportional)),
            (egui::TextStyle::Monospace, FontId::new(10.5, Monospace)),
        ]
        .into();

        let mut v = egui::Visuals::dark();
        v.panel_fill = BG_BOT;
        v.window_fill = CARD;
        v.extreme_bg_color = TRACK;
        v.faint_bg_color = Color32::from_rgb(15, 22, 29);
        v.code_bg_color = TRACK;
        v.override_text_color = Some(TXT);
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;

        v.widgets.noninteractive.bg_fill = CARD;
        v.widgets.noninteractive.weak_bg_fill = CARD;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TXT);
        v.widgets.noninteractive.corner_radius = CornerRadius::same(7);

        v.widgets.inactive.bg_fill = CARD_HI;
        v.widgets.inactive.weak_bg_fill = CARD_HI;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, DIM);
        v.widgets.inactive.corner_radius = CornerRadius::same(7);

        v.widgets.hovered.bg_fill = Color32::from_rgb(32, 46, 58);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(32, 46, 58);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.55));
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, TXT);
        v.widgets.hovered.corner_radius = CornerRadius::same(7);

        v.widgets.active.bg_fill = ACCENT_SOFT;
        v.widgets.active.weak_bg_fill = ACCENT_SOFT;
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.corner_radius = CornerRadius::same(7);

        v.widgets.open.bg_fill = CARD_HI;
        v.widgets.open.weak_bg_fill = CARD_HI;
        v.widgets.open.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.open.fg_stroke = Stroke::new(1.0, TXT);

        style.visuals = v;
    });
    ctx.set_theme(egui::ThemePreference::Dark);
}

// ---------------------------------------------------------------- screenshot

fn save_png(path: &PathBuf, image: &egui::ColorImage) -> Result<(), String> {
    let [w, h] = image.size;
    if w == 0 || h == 0 {
        return Err("empty image".to_string());
    }
    let mut buf = Vec::with_capacity(w * h * 4);
    for pixel in &image.pixels {
        let [r, g, b, _] = pixel.to_array();
        buf.extend_from_slice(&[r, g, b, 255]);
    }
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    image::save_buffer(path, &buf, w as u32, h as u32, image::ColorType::Rgba8)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- main

fn main() -> eframe::Result<()> {
    let mut shot_path: Option<PathBuf> = None;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--screenshot" {
            if let Some(value) = args.get(i + 1) {
                shot_path = Some(PathBuf::from(value));
                i += 1;
            }
        } else if let Some(value) = arg.strip_prefix("--screenshot=") {
            shot_path = Some(PathBuf::from(value));
        } else if arg == "--version" || arg == "-V" {
            println!("ollama-deck {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        } else if arg == "--help" || arg == "-h" {
            println!("ollama-deck {}", env!("CARGO_PKG_VERSION"));
            println!("  load, unload and watch Ollama models");
            println!("  --screenshot <file.png>   draw the widget, save a picture, exit");
            return Ok(());
        }
        i += 1;
    }

    // The screenshot path may run while the session is locked, where Wayland
    // stops delivering frame callbacks and the window never paints. X11 (used
    // through XWayland when present) paints regardless, so prefer it there.
    if shot_path.is_some() && std::env::var_os("DISPLAY").is_some() {
        std::env::remove_var("WAYLAND_DISPLAY");
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Ollama Deck")
        .with_app_id("ollama-deck")
        .with_inner_size(Vec2::new(WIN_W, WIN_H))
        .with_min_inner_size(Vec2::new(350.0, 566.0))
        .with_resizable(true)
        // Keep the window visible while it is being captured: a compositor
        // that hides it refuses to hand out frames and the picture never lands.
        .with_always_on_top();
    if shot_path.is_some() {
        viewport = viewport.with_visible(true);
    }

    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };

    let wants_shot = shot_path.is_some();
    let result = eframe::run_native(
        "Ollama Deck",
        options,
        Box::new(move |cc| Ok(Box::new(Deck::new(cc, shot_path.clone())))),
    );

    if let Err(err) = &result {
        eprintln!("ollama-deck: could not open a window: {err}");
    }
    if wants_shot && result.is_ok() && !SHOT_SAVED.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("ollama-deck: the window closed before a picture was drawn");
        std::process::exit(1);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ollama::LoadReport;

    fn report(ctx: u32, size: u64, vram: u64) -> LoadReport {
        LoadReport {
            resident: true,
            ctx,
            size,
            size_vram: vram,
        }
    }

    #[test]
    fn a_clean_load_says_ready_with_the_context() {
        let (ok, text) = load_message("llama3.2:3b", 8192, false, report(8192, 2_000, 2_000));
        assert!(ok);
        assert_eq!(text, "llama3.2:3b ready · 8k context");
    }

    #[test]
    fn a_capped_context_is_named() {
        let (ok, text) = load_message(
            "qwen2.5-coder:14b",
            65536,
            false,
            report(32768, 2_000, 2_000),
        );
        assert!(ok);
        assert!(text.contains("capped from 64k"), "got {text}");
        assert!(text.contains("32k"), "got {text}");
    }

    #[test]
    fn spill_to_system_ram_is_named() {
        let (_, text) = load_message(
            "qwen2.5-coder:14b",
            32768,
            false,
            report(32768, 14_200_000_000, 13_050_000_000),
        );
        assert!(text.contains("in RAM"), "got {text}");
    }

    #[test]
    fn a_model_that_vanished_is_an_error_not_a_success() {
        let (ok, text) = load_message("llama3.2:3b", 8192, false, LoadReport::default());
        assert!(!ok);
        assert!(text.contains("did not stay in memory"), "got {text}");
    }

    #[test]
    fn a_cloud_model_is_not_expected_to_be_resident() {
        let (ok, text) = load_message("kimi-k2:cloud", 8192, true, LoadReport::default());
        assert!(ok);
        assert!(text.contains("cloud"), "got {text}");
    }
}
