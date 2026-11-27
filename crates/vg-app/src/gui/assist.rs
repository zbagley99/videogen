//! The pose suggestions window: an LLM reads the script and proposes pose
//! tags (as structured JSON, checked and repaired against the script and the
//! library); you review and apply them. Only the script's words and the pose
//! library's names and descriptions are sent, to the one server chosen here.

use super::util::plural;
use super::{place, VgApp, theme};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use vg_llm::{Mode, Note, SuggestOptions, Suggestion};

#[derive(Default)]
pub struct AssistPanel {
    pub open: bool,
    task: Option<SuggestTask>,
    /// Suggestions for `for_text`, each with its tick box.
    results: Vec<(Suggestion, bool)>,
    notes: Vec<Note>,
    for_text: String,
    mode_used: Option<Mode>,
    /// The script before the last apply, for Undo.
    before_apply: Option<String>,
    /// What Test found on each server: its models, or the error.
    tested: std::collections::HashMap<String, Result<String, String>>,
    /// A server being typed in, to add to the list.
    adding: String,
    summary: String,
}

/// What a suggestion run returns: the suggestions, notes, and the model's raw reply.
type Answer = (Vec<Suggestion>, Vec<Note>, String);

struct SuggestTask {
    handle: std::thread::JoinHandle<anyhow::Result<Answer>>,
    progress: Arc<Mutex<(usize, usize)>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    text: String,
    mode: Mode,
}

impl AssistPanel {
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
}

/// Start suggesting for the current script (also used after transcribing).
pub fn start(app: &mut VgApp, ctx: &egui::Context, mode: Mode) {
    if app.assist.task.is_some() {
        return;
    }
    if app.poses.poses.is_empty() {
        app.error("load a pose library first");
        return;
    }
    let (poses, text, url) = (app.poses.clone(), app.cue_text.clone(), app.prefs.llm_url.clone());
    let opts = SuggestOptions { mode, style: app.prefs.llm_style.clone(), ..Default::default() };
    let progress = Arc::new(Mutex::new((0, 0)));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c, ctx2, t) = (progress.clone(), cancel.clone(), ctx.clone(), text.clone());
    let handle = std::thread::spawn(move || {
        let mut client = vg_llm::Client::new(&url);
        let r = vg_llm::suggest(
            &mut client,
            &t,
            &poses,
            &opts,
            &mut |d, n| {
                *p.lock().unwrap() = (d, n);
                ctx2.request_repaint();
            },
            &|| c.load(Ordering::Relaxed),
        );
        ctx2.request_repaint();
        let (s, n) = r?;
        Ok((s, n, format!("{} ({:?})", client.model.unwrap_or_default(), client.structure)))
    });
    app.assist.open = true;
    app.assist.task = Some(SuggestTask { handle, progress, cancel, started: Instant::now(), text, mode });
}

/// Collect finished work.
fn poll(app: &mut VgApp) {
    if app.assist.task.as_ref().is_some_and(|t| t.handle.is_finished()) {
        let task = app.assist.task.take().unwrap();
        match task.handle.join() {
            Ok(Ok((sugg, notes, model))) => {
                let (fixed, dropped) = (notes.iter().filter(|n| n.fixed).count(), notes.iter().filter(|n| !n.fixed).count());
                app.assist.summary = format!("{} suggestions from {model} in {:.0} s · {fixed} fixed · {dropped} dropped", sugg.len(), task.started.elapsed().as_secs_f64());
                app.assist.results = sugg.into_iter().map(|s| (s, true)).collect();
                app.assist.notes = notes;
                app.assist.for_text = task.text;
                app.assist.mode_used = Some(task.mode);
            }
            Ok(Err(e)) => app.error(format!("suggestions: {e:#}")),
            Err(_) => app.error("the suggestion task crashed"),
        }
    }
}

fn apply(app: &mut VgApp) {
    if app.cue_text != app.assist.for_text {
        app.error("the script changed since these suggestions were made; suggest again");
        return;
    }
    let chosen: Vec<Suggestion> = app.assist.results.iter().filter(|(_, on)| *on).map(|(s, _)| s.clone()).collect();
    let mode = app.assist.mode_used.unwrap_or(Mode::Untagged);
    match vg_llm::suggest::apply(&app.cue_text, &chosen, &app.poses, mode) {
        Ok(text) => {
            app.assist.before_apply = Some(std::mem::replace(&mut app.cue_text, text));
            app.reparse();
            app.assist.results.clear();
            app.info(format!("added {} pose tag{} (Ctrl+S saves)", chosen.len(), plural(chosen.len())));
        }
        Err(e) => app.error(format!("{e:#}")),
    }
}

pub fn window(app: &mut VgApp, ctx: &egui::Context) {
    poll(app);
    if !app.assist.open {
        return;
    }
    let mut open = app.assist.open;
    let pos = app.place.place(ctx, "suggest", egui::Align2::CENTER_TOP, egui::vec2(640.0, 560.0));
    place::at(egui::Window::new(theme::caption("Pose suggestions").color(theme::WHITE)), "suggest", pos)
        .open(&mut open)
        .resizable(true)
        .collapsible(false)
        .default_width(620.0)
        .default_height(560.0)
        .show(ctx, |ui| body(app, ui, ctx));
    app.assist.open = open;
}

fn body(app: &mut VgApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    let busy = app.assist.task.is_some();
    // The one server the script goes to.
    ui.label(theme::caption("Server"));
    let mut remove = None;
    ui.add_enabled_ui(!busy, |ui| {
        egui::Grid::new("llm-servers").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
            for (k, url) in app.prefs.llm_endpoints.iter().enumerate() {
                ui.radio_value(&mut app.prefs.llm_url, url.clone(), egui::RichText::new(url).monospace().size(11.5));
                tested(ui, app.assist.tested.get(url));
                if app.prefs.llm_endpoints.len() > 1 && theme::tool_button(ui, "Remove", None, false).clicked() {
                    remove = Some(k);
                }
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.assist.adding).hint_text("another server, e.g. http://host:8080/v1").desired_width(300.0));
            let url = app.assist.adding.trim().trim_end_matches('/').to_string();
            if theme::tool_button_enabled(ui, "Add", None, false, !url.is_empty() && !app.prefs.llm_endpoints.contains(&url)).clicked() {
                app.prefs.llm_endpoints.push(url.clone());
                app.prefs.llm_url = url;
                app.assist.adding.clear();
            }
            if theme::tool_button(ui, "Test all", None, false).clicked() {
                for url in &app.prefs.llm_endpoints {
                    let r = vg_llm::Client::new(url).models().map(|m| m.join(", ")).map_err(|e| format!("{e:#}"));
                    app.assist.tested.insert(url.clone(), r);
                }
            }
        });
    });
    if let Some(k) = remove {
        let gone = app.prefs.llm_endpoints.remove(k);
        if app.prefs.llm_url == gone {
            app.prefs.llm_url = app.prefs.llm_endpoints[0].clone();
        }
    }
    if !app.prefs.llm_endpoints.contains(&app.prefs.llm_url) {
        app.prefs.llm_endpoints.push(app.prefs.llm_url.clone());
    }
    theme::weak(ui, "Only the script's words and the pose names and descriptions are sent, to the selected server.");
    ui.separator();

    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal(|ui| {
            ui.label(theme::caption("Lines"));
            theme::segmented(ui, &mut app.prefs.llm_replace, &[(false, "Untagged only"), (true, "All (replace tags)")]);
        });
        ui.horizontal(|ui| {
            ui.label(theme::caption("Direction"));
            ui.add(egui::TextEdit::singleline(&mut app.prefs.llm_style).hint_text("optional, e.g. more reactions, fewer waves").desired_width(f32::INFINITY));
        });
    });
    ui.horizontal(|ui| match &app.assist.task {
        Some(t) => {
            let (d, total) = *t.progress.lock().unwrap();
            let text = if total > 0 { format!("batch {}/{total} · {:.0} s", (d + 1).min(total), t.started.elapsed().as_secs_f64()) } else { format!("connecting · {:.0} s", t.started.elapsed().as_secs_f64()) };
            ui.add(egui::ProgressBar::new(if total > 0 { d as f32 / total as f32 } else { 0.0 }).desired_width(260.0).text(text).animate(true));
            if theme::tool_button(ui, "Cancel", None, false).clicked() {
                t.cancel.store(true, Ordering::Relaxed);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        None => {
            if theme::tool_button(ui, "Suggest poses", Some("Ctrl+G"), true).clicked() {
                let mode = if app.prefs.llm_replace { Mode::Replace } else { Mode::Untagged };
                start(app, ctx, mode);
            }
            if let Some(before) = app.assist.before_apply.clone()
                && theme::tool_button(ui, "Undo apply", None, false).clicked() {
                    app.cue_text = before;
                    app.assist.before_apply = None;
                    app.reparse();
                }
        }
    });

    if !app.assist.summary.is_empty() {
        ui.add_space(4.0);
        theme::weak(ui, app.assist.summary.clone());
    }
    if !app.assist.results.is_empty() {
        let stale = app.cue_text != app.assist.for_text;
        ui.add_space(4.0);
        let mut seek = None;
        egui::ScrollArea::vertical().max_height(ui.available_height() - 90.0).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("suggestions").num_columns(4).striped(false).spacing([10.0, 6.0]).show(ui, |ui| {
                for (s, on) in &mut app.assist.results {
                    ui.checkbox(on, "");
                    let tag = &app.poses.poses[s.pose].tag;
                    ui.label(egui::RichText::new(format!("[{tag}]")).monospace().color(if *on { theme::MARK } else { theme::MUTED }));
                    // The line with the switch point marked.
                    let mut job = egui::text::LayoutJob::default();
                    let words: Vec<&str> = s.line_text.split_whitespace().collect();
                    let font = egui::FontId::proportional(12.0);
                    job.append(&format!("{:>3}  ", s.line_no), 0.0, egui::TextFormat { font_id: egui::FontId::monospace(10.5), color: theme::MUTED, ..Default::default() });
                    for (k, w) in words.iter().enumerate() {
                        if k == s.word {
                            job.append("› ", 0.0, egui::TextFormat { font_id: font.clone(), color: theme::MARK, ..Default::default() });
                        }
                        job.append(&format!("{w} "), 0.0, egui::TextFormat { font_id: font.clone(), color: if k == s.word { theme::WHITE } else { theme::TEXT2 }, ..Default::default() });
                    }
                    job.wrap.max_width = 330.0;
                    if ui.add(egui::Label::new(job).sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        seek = Some(s.line_no);
                    }
                    theme::note(ui, &s.reason);
                    ui.end_row();
                }
            });
        });
        if let Some(line_no) = seek
            && let Some(t) = app.parsed.timeline.as_ref().and_then(|tl| tl.spans.iter().find(|sp| sp.line_no == line_no).map(|sp| sp.start)) {
                app.seek(t);
            }
        ui.horizontal(|ui| {
            let chosen = app.assist.results.iter().filter(|(_, on)| *on).count();
            if theme::tool_button_enabled(ui, &format!("Apply {chosen}"), None, true, chosen > 0 && !stale).clicked() {
                apply(app);
            }
            if theme::tool_button(ui, "All", None, false).clicked() {
                app.assist.results.iter_mut().for_each(|r| r.1 = true);
            }
            if theme::tool_button(ui, "None", None, false).clicked() {
                app.assist.results.iter_mut().for_each(|r| r.1 = false);
            }
            if stale {
                theme::weak(ui, "The script changed since; suggest again.");
            } else if app.assist.mode_used == Some(Mode::Replace) {
                theme::weak(ui, "Applying replaces the script's pose tags.");
            }
        });
    }
    if !app.assist.notes.is_empty() {
        egui::CollapsingHeader::new(theme::caption(format!("Fixes and drops · {}", app.assist.notes.len()))).default_open(false).show(ui, |ui| {
            egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                for n in &app.assist.notes {
                    ui.label(egui::RichText::new(format!("{} {}", if n.fixed { "fixed ·" } else { "dropped ·" }, n.text)).size(10.5).color(if n.fixed { theme::MUTED } else { theme::TEXT2 }));
                }
            });
        });
    }
}

fn tested(ui: &mut egui::Ui, r: Option<&Result<String, String>>) {
    match r {
        Some(Ok(m)) => {
            ui.add(egui::Label::new(egui::RichText::new(m).size(10.5).color(theme::WHITE)).truncate());
        }
        Some(Err(e)) => {
            ui.add(egui::Label::new(egui::RichText::new(format!("Error · {e}")).size(10.5).color(theme::TEXT2)).truncate());
        }
        None => {
            ui.label("");
        }
    }
}
