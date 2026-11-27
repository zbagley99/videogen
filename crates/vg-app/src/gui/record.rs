//! The record window: record narration takes from a microphone into WAV files
//! next to the script, then use a take as the narration (and transcribe it).

use super::util::file_name;
use super::{place, VgApp, theme};
use crate::recorder::{self, PEAK_RATE, Recording, Take};
use eframe::egui;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const COUNTDOWN: Duration = Duration::from_secs(3);
/// Seconds of waveform shown while recording.
const WAVE_SECONDS: usize = 8;

/// What a finished take becomes.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Target {
    /// The script's narration (and transcribed, if that's ticked).
    #[default]
    Narration,
    /// A new track in the Audio tab's edit, starting at this time.
    Track(f64),
}

#[derive(Default)]
pub struct RecordPanel {
    pub open: bool,
    target: Target,
    /// Input device names, listed the first time the window opens.
    devices: Option<Vec<String>>,
    rec: Option<Recording>,
    /// When the countdown ends and the take starts being written.
    go_at: Option<Instant>,
    /// Meter level in dBFS, smoothed for display, and the held peak.
    meter_db: f32,
    hold: (f32, Option<Instant>),
    /// Takes on disk for the current script: (folder, stem) they were listed for.
    takes: Vec<Take>,
    listed_for: Option<(PathBuf, String)>,
}

impl RecordPanel {
    pub fn recording(&self) -> Option<&Recording> {
        self.rec.as_ref()
    }
}

/// Where takes go and what they're called: next to the script, named after it.
fn take_place(app: &VgApp) -> (PathBuf, String) {
    let stem = app.cue_path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "narration".into());
    (app.cue_dir(), stem)
}

fn refresh_takes(app: &mut VgApp, force: bool) {
    let place = take_place(app);
    if force || app.record.listed_for.as_ref() != Some(&place) {
        let current = app.record.rec.as_ref().map(|r| r.path.clone());
        app.record.takes = recorder::takes(&place.0, &place.1).into_iter().filter(|t| Some(&t.path) != current.as_ref()).collect();
        app.record.listed_for = Some(place);
    }
}

/// Ctrl+R: open the window; with it open, start or stop a take.
pub fn shortcut(app: &mut VgApp, ctx: &egui::Context) {
    if !app.record.open {
        app.record.open = true;
    } else if app.record.rec.is_some() {
        stop(app, ctx);
    } else {
        start(app);
    }
}

/// Record a new track into the Audio tab's edit, at its playhead.
pub fn start_track(app: &mut VgApp) {
    if app.record.rec.is_some() {
        return app.error("a take is already recording");
    }
    app.record.target = Target::Track(app.studio.time);
    app.record.open = true;
    start(app);
}

pub fn start(app: &mut VgApp) {
    app.stop(); // don't record the speakers
    app.studio.stop();
    let (dir, stem) = take_place(app);
    let path = recorder::next_take_path(&dir, &stem);
    let countdown = app.prefs.record_countdown;
    match Recording::start(app.prefs.record_device.as_deref(), &path, !countdown) {
        Ok(rec) => {
            app.record.rec = Some(rec);
            app.record.go_at = countdown.then(|| Instant::now() + COUNTDOWN);
            app.record.hold = (-120.0, None);
        }
        Err(e) => app.error(format!("{e:#}")),
    }
}

/// Stop and keep the take (a take stopped during the countdown is thrown away).
pub fn stop(app: &mut VgApp, ctx: &egui::Context) {
    let Some(rec) = app.record.rec.take() else { return };
    app.record.go_at = None;
    let target = std::mem::take(&mut app.record.target);
    if !rec.armed() {
        rec.discard();
        app.info("recording cancelled");
        return;
    }
    match rec.finish() {
        Ok(take) => {
            refresh_takes(app, true);
            app.info(format!("recorded {} ({})", file_name(&take.path), vg_core::format_time(take.duration)));
            match target {
                Target::Narration => use_take(app, ctx, &take.path, app.prefs.record_transcribe),
                Target::Track(at) => {
                    super::studio::add_track(app, &take.path, true, Some(at));
                    app.record.open = false;
                }
            }
        }
        Err(e) => app.error(format!("{e:#}")),
    }
}

/// Throw the take in progress away.
pub fn discard(app: &mut VgApp) {
    if let Some(rec) = app.record.rec.take() {
        rec.discard();
        app.record.target = Target::Narration;
        app.record.go_at = None;
        app.info("take discarded");
    }
}

/// Make `path` the narration. Transcribing writes a new script that already
/// points at it, so the current script is only touched when not transcribing.
fn use_take(app: &mut VgApp, ctx: &egui::Context, path: &std::path::Path, transcribe: bool) {
    if transcribe {
        super::transcribe::open_for(app, path.to_path_buf());
        super::transcribe::start(app, ctx);
    } else {
        app.set_narration(path);
        app.use_saved_words(path);
    }
}

pub fn window(app: &mut VgApp, ctx: &egui::Context) {
    // The countdown ends: start writing.
    if let (Some(go), Some(rec)) = (app.record.go_at, &app.record.rec)
        && Instant::now() >= go {
            rec.arm();
            app.record.go_at = None;
        }
    if let Some(e) = app.record.rec.as_ref().and_then(|r| r.shared().error()) {
        stop(app, ctx);
        app.error(format!("input: {e}"));
    }
    if !app.record.open {
        // Closing the window keeps the take.
        if app.record.rec.is_some() {
            stop(app, ctx);
        }
        app.record.target = Target::Narration;
        return;
    }
    if app.record.devices.is_none() {
        app.record.devices = Some(recorder::input_names().unwrap_or_default());
    }
    refresh_takes(app, false);

    // Meter: fast attack, slow release, and a 1.5 s peak hold.
    let peak = app.record.rec.as_ref().map(|r| r.shared().take_peak()).unwrap_or(0.0);
    let db = 20.0 * peak.max(1e-6).log10();
    let m = &mut app.record.meter_db;
    *m = if db > *m { db } else { (*m - 1.2).max(db).max(-60.0) };
    if db >= app.record.hold.0 || app.record.hold.1.is_none_or(|t| t.elapsed() > Duration::from_millis(1500)) {
        app.record.hold = (db, Some(Instant::now()));
    }

    let mut open = app.record.open;
    let busy = app.record.rec.is_some();
    let pos = app.place.place(ctx, "record", egui::Align2::RIGHT_TOP, egui::vec2(450.0, 470.0));
    let title = match app.record.target {
        Target::Narration => "Record narration",
        Target::Track(_) => "Record a new track",
    };
    place::at(egui::Window::new(theme::caption(title).color(theme::WHITE)), "record", pos)
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            ui.set_width(420.0);
            ui.add_enabled_ui(!busy, |ui| {
                egui::Grid::new("record").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(theme::caption("Input"));
                    let current = app.prefs.record_device.clone().unwrap_or_else(|| "System default".into());
                    egui::ComboBox::from_id_salt("input").width(260.0).selected_text(current).show_ui(ui, |ui| {
                        ui.selectable_value(&mut app.prefs.record_device, None, "System default");
                        for d in app.record.devices.clone().unwrap_or_default() {
                            ui.selectable_value(&mut app.prefs.record_device, Some(d.clone()), d);
                        }
                    });
                    ui.end_row();
                    ui.label(theme::caption("Countdown"));
                    theme::segmented(ui, &mut app.prefs.record_countdown, &[(false, "Off"), (true, "3 s")]);
                    ui.end_row();
                    ui.label(theme::caption("When done"));
                    ui.checkbox(&mut app.prefs.record_transcribe, "Transcribe the take into a new script");
                    ui.end_row();
                });
                if let Target::Track(at) = app.record.target {
                    theme::note(ui, format!("This take becomes a new track in the Audio tab, starting at {}, with the same clean-up as the narration.", vg_core::format_time(at)));
                }
                match &app.record.rec {
                    Some(rec) => theme::note(ui, format!("Recording to {} from {} at {} Hz.", file_name(&rec.path), rec.device, rec.rate)),
                    None => {
                        let (dir, stem) = take_place(app);
                        let next = recorder::next_take_path(&dir, &stem);
                        theme::note(ui, format!("Saves {} next to the script. Stopped takes become the narration.", file_name(&next)));
                    }
                }
            });
            ui.add_space(6.0);
            meter(ui, app.record.meter_db, app.record.hold.0, app.record.rec.as_ref().is_some_and(|r| r.shared().clipped()), busy);
            ui.add_space(4.0);
            let peaks = app.record.rec.as_ref().map(|r| r.shared().recent_peaks(WAVE_SECONDS * PEAK_RATE as usize)).unwrap_or_default();
            waveform(ui, &peaks);
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                let elapsed = app.record.rec.as_ref().map(|r| r.elapsed()).unwrap_or(0.0);
                ui.label(egui::RichText::new(vg_core::format_time(elapsed)).monospace().size(22.0).color(if busy { theme::WHITE } else { theme::MUTED }));
                let state = match (&app.record.rec, app.record.go_at) {
                    (Some(_), Some(go)) => format!("STARTING IN {}", (go - Instant::now().min(go)).as_secs_f32().ceil() as u32),
                    (Some(_), None) => "REC".into(),
                    (None, _) => "READY".into(),
                };
                ui.add_space(10.0);
                let r = ui.label(theme::caption(&state).color(if busy { theme::WHITE } else { theme::MUTED }));
                if busy && app.record.go_at.is_none() {
                    // A plain square "recording" light next to the label.
                    let c = r.rect.left_center() - egui::vec2(8.0, 0.0);
                    ui.painter().rect_filled(egui::Rect::from_center_size(c, egui::vec2(6.0, 6.0)), 0.0, theme::WHITE);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if busy {
                        if theme::tool_button(ui, "Discard", None, false).clicked() {
                            discard(app);
                        }
                        let stop_label = if matches!(app.record.target, Target::Track(_)) { "Stop and add track" } else { "Stop" };
                        if theme::tool_button(ui, stop_label, Some("Ctrl+R"), true).clicked() {
                            stop(app, ctx);
                        }
                    } else if theme::tool_button(ui, "Record", Some("Ctrl+R"), false).clicked() {
                        start(app);
                    }
                });
            });

            if !app.record.takes.is_empty() {
                ui.separator();
                ui.label(theme::caption(format!("Takes · {}", app.record.takes.len())).color(theme::TEXT2));
                let in_use = app.parsed.audio_path.as_deref().map(vg_core::paths::canon);
                let mut action = None;
                egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                    for take in app.record.takes.iter().rev() {
                        ui.horizontal(|ui| {
                            let name = file_name(&take.path);
                            let used = in_use.as_ref().is_some_and(|u| std::fs::canonicalize(&take.path).is_ok_and(|t| &t == u));
                            ui.label(egui::RichText::new(name).color(if used { theme::WHITE } else { theme::TEXT2 }));
                            ui.label(egui::RichText::new(vg_core::format_time(take.duration)).color(theme::MUTED));
                            if used {
                                ui.label(theme::caption("in use").color(theme::WHITE));
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_enabled_ui(!busy, |ui| {
                                    if theme::tool_button(ui, "Transcribe", None, false).clicked() {
                                        action = Some((take.path.clone(), true));
                                    }
                                    if !used && theme::tool_button(ui, "Use", None, false).clicked() {
                                        action = Some((take.path.clone(), false));
                                    }
                                });
                            });
                        });
                    }
                });
                if let Some((path, transcribe)) = action {
                    use_take(app, ctx, &path, transcribe);
                }
            }
        });
    app.record.open = open;
    if busy {
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

/// A horizontal dBFS meter from −60 to 0 with the held peak as a tick.
fn meter(ui: &mut egui::Ui, db: f32, hold: f32, clipped: bool, live: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::hover());
    let p = ui.painter();
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() - 64.0, 12.0));
    let x = |db: f32| bar.left() + ((db + 60.0) / 60.0).clamp(0.0, 1.0) * bar.width();
    p.rect_stroke(bar, 0.0, egui::Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Inside);
    if live {
        // White up to −6 dB (a good speaking peak), grey above it.
        let fill = egui::Rect::from_min_max(bar.min, egui::pos2(x(db), bar.bottom())).shrink(2.0);
        p.rect_filled(fill, 0.0, theme::WHITE);
        let hot = egui::Rect::from_min_max(egui::pos2(x(-6.0), bar.top()), egui::pos2(x(db), bar.bottom())).shrink(2.0);
        if hot.width() > 0.0 {
            p.rect_filled(hot, 0.0, theme::TEXT2);
        }
        if hold > -60.0 {
            let hx = x(hold);
            p.line_segment([egui::pos2(hx, bar.top() + 1.0), egui::pos2(hx, bar.bottom() - 1.0)], egui::Stroke::new(2.0_f32, theme::WHITE));
        }
    }
    for mark in [-48.0, -36.0, -24.0, -12.0, -6.0, 0.0] {
        let mx = x(mark);
        p.line_segment([egui::pos2(mx, bar.bottom()), egui::pos2(mx, bar.bottom() + 4.0)], egui::Stroke::new(1.0_f32, theme::RULE));
        p.text(egui::pos2(mx, bar.bottom() + 5.0), egui::Align2::CENTER_TOP, format!("{}", mark as i32), egui::FontId::monospace(8.5), theme::MUTED);
    }
    let readout = if live { format!("{db:>5.1} dB") } else { "  — dB".into() };
    p.text(egui::pos2(rect.right(), bar.center().y), egui::Align2::RIGHT_CENTER, readout, egui::FontId::monospace(11.0), if live { theme::WHITE } else { theme::MUTED });
    if clipped {
        let r = egui::Rect::from_min_size(egui::pos2(rect.right() - 40.0, bar.bottom() + 3.0), egui::vec2(40.0, 13.0));
        theme::dashed_rect(p, r, egui::Stroke::new(1.0_f32, theme::WHITE));
        p.text(r.center(), egui::Align2::CENTER_CENTER, "CLIP", egui::FontId::monospace(9.0), theme::WHITE);
    }
}

/// The last few seconds of the take, newest on the right.
fn waveform(ui: &mut egui::Ui, peaks: &[f32]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 56.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, theme::RULE), egui::StrokeKind::Inside);
    let mid = rect.center().y;
    p.line_segment([egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)], egui::Stroke::new(1.0_f32, theme::RULE_SOFT));
    if peaks.is_empty() {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, "the take's waveform shows here", egui::FontId::monospace(10.0), theme::MUTED);
        return;
    }
    let cols = rect.width().floor() as usize;
    let per_col = (WAVE_SECONDS * PEAK_RATE as usize) as f32 / cols as f32;
    let half = rect.height() / 2.0 - 3.0;
    let n = peaks.len();
    for c in 0..cols {
        // Column c covers peaks ending `cols - c` columns before the newest.
        let end = n as f32 - (cols - 1 - c) as f32 * per_col;
        if end <= 0.0 {
            continue;
        }
        let (a, b) = (((end - per_col).max(0.0)) as usize, (end as usize).min(n));
        let v = peaks[a..b.max(a + 1).min(n)].iter().copied().fold(0.0f32, f32::max);
        // Square-root scale so speech reads clearly without being clipped.
        let hgt = (v.sqrt() * half).max(0.5);
        let x = rect.left() + c as f32 + 0.5;
        p.line_segment([egui::pos2(x, mid - hgt), egui::pos2(x, mid + hgt)], egui::Stroke::new(1.0_f32, theme::TEXT2));
    }
}
