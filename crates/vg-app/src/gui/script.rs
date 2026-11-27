//! The script pane. By default a list of the spoken lines: a timecode column
//! (drag or type to retime) and each line's words with its pose tags, edited in
//! place. Comments are hidden and the `@` directives show as details in a
//! footer. The Source view is the whole cue sheet as text. Also helpers that
//! edit the text for the palette and the timeline.

use super::util::{file_name, plural};
use super::{VgApp, theme};
use eframe::egui;
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use std::collections::HashSet;
use std::sync::Arc;
use vg_core::format_time;

pub struct ScriptEditor {
    id: egui::Id,
    /// Move the cursor to this 1-based line next frame (after a timeline click).
    jump_to_line: Option<usize>,
    /// The line last edited in the line list (1-based).
    focused: Option<usize>,
    /// The line list is showing (not the source).
    lines_mode: bool,
}

impl Default for ScriptEditor {
    fn default() -> Self {
        ScriptEditor { id: egui::Id::new("script-editor"), jump_to_line: None, focused: None, lines_mode: true }
    }
}

/// The text box of line `line_no` in the line list.
fn row_id(line_no: usize) -> egui::Id {
    egui::Id::new(("script-line", line_no))
}

impl ScriptEditor {
    pub fn jump_to_line(&mut self, line_no: usize) {
        self.jump_to_line = Some(line_no);
    }

    /// Insert `s` at the text cursor (or the end), padding with spaces, and leave the cursor after it.
    /// In the line list, it goes into the line being edited.
    pub fn insert_at_cursor(&mut self, ctx: &egui::Context, text: &mut String, s: &str) {
        // The line list: into the line last edited (clicking a pose takes the focus away).
        if let Some(line_no) = self.focused.filter(|_| self.lines_mode)
            && let Some(row) = parse_lines(text).into_iter().find(|r| r.line_no == line_no) {
                let mut state = egui::text_edit::TextEditState::load(ctx, row_id(line_no)).unwrap_or_default();
                let mut body = row.body.clone();
                let char_idx = state.cursor.char_range().map(|r| r.primary.index).unwrap_or_else(|| body.chars().count());
                let byte = body.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(body.len());
                let ins = padded(&body, byte, s);
                body.insert_str(byte, &ins);
                set_line(text, &row, row.time, &body);
                state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(char_idx + ins.chars().count()))));
                state.store(ctx, row_id(line_no));
                ctx.memory_mut(|m| m.request_focus(row_id(line_no)));
                return;
            }
        let mut state = egui::text_edit::TextEditState::load(ctx, self.id).unwrap_or_default();
        let char_idx = state.cursor.char_range().map(|r| r.primary.index).unwrap_or_else(|| text.chars().count());
        let byte = text.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(text.len());
        let ins = padded(text, byte, s);
        text.insert_str(byte, &ins);
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(char_idx + ins.chars().count()))));
        state.store(ctx, self.id);
        ctx.memory_mut(|m| m.request_focus(self.id));
    }
}

/// `s` with a space before and after where it would touch other text at `byte`.
fn padded(text: &str, byte: usize, s: &str) -> String {
    let mut ins = String::new();
    if text[..byte].chars().last().is_some_and(|c| !c.is_whitespace()) {
        ins.push(' ');
    }
    ins.push_str(s);
    if text[byte..].chars().next().is_some_and(|c| !c.is_whitespace()) {
        ins.push(' ');
    }
    ins
}

/// A spoken line of the script: its explicit time (if any) and the rest.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    line_no: usize,
    time: Option<f64>,
    body: String,
}

/// What's in the script besides the spoken lines.
#[derive(Default)]
struct Details {
    comments: usize,
    directives: Vec<(String, String)>,
}

/// The spoken lines (not comments, directives or blank lines), read from the
/// text line by line, so a script with errors still shows.
fn parse_lines(text: &str) -> Vec<Row> {
    text.split('\n')
        .enumerate()
        .filter_map(|(i, raw)| {
            let line = raw.trim_end_matches('\r');
            let t = line.trim_start();
            if t.is_empty() || t.starts_with('#') || t.starts_with("//") || t.starts_with('@') {
                return None;
            }
            // A leading `[time]`.
            if let Some(rest) = t.strip_prefix('[')
                && let Some(close) = rest.find(']')
                    && let Some(time) = vg_core::parse_time(&rest[..close]) {
                        return Some(Row { line_no: i + 1, time: Some(time), body: rest[close + 1..].trim_start().to_string() });
                    }
            Some(Row { line_no: i + 1, time: None, body: t.to_string() })
        })
        .collect()
}

fn details(text: &str) -> Details {
    let mut d = Details::default();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('#') || t.starts_with("//") {
            d.comments += 1;
        } else if let Some(rest) = t.strip_prefix('@') {
            let (k, v) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            d.directives.push((k.to_string(), v.split(" #").next().unwrap_or("").trim().to_string()));
        }
    }
    d
}

/// Rewrite a spoken line as `[time] body` (the body may hold newlines: the
/// lines after the first become new, untimed lines).
fn set_line(text: &mut String, row: &Row, time: Option<f64>, body: &str) {
    let Some((start, len)) = line_bounds(text, row.line_no) else { return };
    let mut parts = body.split('\n');
    let first = parts.next().unwrap_or("").trim();
    let mut out = match time {
        Some(t) => format!("[{}] {first}", format_time(t)),
        None => first.to_string(),
    };
    for p in parts {
        out.push('\n');
        out.push_str(p.trim());
    }
    text.replace_range(start..start + len, &out);
}

/// Remove spoken line `line_no`.
fn delete_line(text: &mut String, line_no: usize) {
    if let Some((start, len)) = line_bounds(text, line_no) {
        let end = (start + len + 1).min(text.len());
        text.replace_range(start..end, "");
    }
}

/// Add an empty line after `line_no` (with a time, if given).
fn insert_line_after(text: &mut String, line_no: usize, time: Option<f64>) -> usize {
    let new = match time {
        Some(t) => format!("[{}] ", format_time(t)),
        None => String::new(),
    };
    match line_bounds(text, line_no) {
        Some((start, len)) => {
            text.insert_str(start + len, &format!("\n{new}"));
            line_no + 1
        }
        None => {
            if !text.ends_with('\n') && !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&new);
            text.lines().count()
        }
    }
}

/// Byte offset where 1-based line `line_no` starts, and the line's length.
fn line_bounds(text: &str, line_no: usize) -> Option<(usize, usize)> {
    let mut start = 0;
    for (i, line) in text.split('\n').enumerate() {
        if i + 1 == line_no {
            return Some((start, line.trim_end_matches('\r').len()));
        }
        start += line.len() + 1;
    }
    None
}

/// Append `[tag@time]` to line `line_no`, or add a new timed line when there is none.
pub fn insert_timed_tag(text: &mut String, line_no: Option<usize>, tag: &str, t: f64) {
    let tagtext = format!("[{tag}@{}]", format_time(t));
    match line_no.and_then(|l| line_bounds(text, l)) {
        Some((start, len)) => {
            let end = start + len;
            let pad = if text[start..end].ends_with(' ') { "" } else { " " };
            text.insert_str(end, &format!("{pad}{tagtext}"));
        }
        None => {
            if !text.ends_with('\n') && !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!("[{}] [{tag}]\n", format_time(t)));
        }
    }
}

/// Rewrite the tag at `range` on `line_no` to `[name@time]`.
pub fn retime_tag(text: &mut String, line_no: usize, range: (usize, usize), name: &str, t: f64) -> bool {
    let Some((start, len)) = line_bounds(text, line_no) else { return false };
    if range.1 > len {
        return false;
    }
    let (a, b) = (start + range.0, start + range.1);
    if !text[a..b].starts_with('[') {
        return false;
    }
    text.replace_range(a..b, &format!("[{name}@{}]", format_time(t.max(0.0))));
    true
}

/// Delete the tag at `range` on `line_no` (and one adjacent space).
pub fn delete_tag(text: &mut String, line_no: usize, range: (usize, usize)) -> bool {
    let Some((start, len)) = line_bounds(text, line_no) else { return false };
    if range.1 > len {
        return false;
    }
    let (mut a, mut b) = (start + range.0, start + range.1);
    if text[b..].starts_with(' ') {
        b += 1;
    } else if a > start && text[..a].ends_with(' ') {
        a -= 1;
    }
    text.replace_range(a..b, "");
    true
}

pub fn show(app: &mut VgApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    ui.horizontal(|ui| {
        let name = app.cue_path.as_deref().map(file_name).unwrap_or_else(|| "not saved".into());
        let dirty = app.cue_text != app.cue_saved_text;
        theme::panel_title(ui, "Script", &format!("{name}{}", if dirty { " · edited" } else { "" }));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::tool_button_enabled(ui, "Save", None, false, dirty || app.cue_path.is_none()).clicked()
                && let Err(e) = app.save_script() {
                    app.error(format!("{e:#}"));
                }
            theme::segmented(ui, &mut app.prefs.script_source, &[(true, "Source"), (false, "Lines")]);
        });
    });
    theme::note(ui, "[pose] switches · [pose@0:05.2] pins a time · [hide] [show]");
    ui.add_space(4.0);

    // Errors first, so they're visible without scrolling: words plus a dashed edge.
    let errors = app.parsed.errors.clone();
    if !errors.is_empty() {
        let inner = egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for e in errors.iter().take(6) {
                let text = if e.line_no > 0 { format!("Error · line {}: {}", e.line_no, e.message) } else { format!("Error · {}", e.message) };
                let r = ui.add(egui::Label::new(egui::RichText::new(text).size(11.5).color(theme::WHITE)).sense(egui::Sense::click()));
                if r.clicked() && e.line_no > 0 {
                    app.script.jump_to_line(e.line_no);
                }
            }
            if errors.len() > 6 {
                theme::note(ui, format!("{} more", errors.len() - 6));
            }
            theme::note(ui, "Click an error to go to its line. The preview keeps the last good version.");
        });
        theme::dashed_rect(ui.painter(), inner.response.rect, egui::Stroke::new(1.0_f32, theme::BORDER));
        ui.add_space(4.0);
    }

    let mut names: HashSet<String> = app.poses.names().map(str::to_string).collect();
    names.insert("hide".into());
    names.insert("show".into());
    let error_lines: HashSet<usize> = errors.iter().map(|e| e.line_no).collect();
    let playing_line = app.parsed.timeline.as_ref().and_then(|tl| tl.spans.iter().rev().find(|s| s.start <= app.time && app.time < s.end)).map(|s| s.line_no);

    egui::Panel::bottom("script-details").frame(egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 0 })).show_inside(ui, |ui| footer(app, ui));

    let before = app.cue_text.clone();
    app.script.lines_mode = !app.prefs.script_source;
    if app.prefs.script_source {
        source_view(app, ui, &ctx, &names, &error_lines, playing_line);
    } else {
        lines_view(app, ui, &ctx, &names, &error_lines, playing_line);
    }
    if app.cue_text != before {
        app.reparse();
    }
}

fn source_view(app: &mut VgApp, ui: &mut egui::Ui, ctx: &egui::Context, names: &HashSet<String>, error_lines: &HashSet<usize>, playing_line: Option<usize>) {
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| -> Arc<egui::Galley> {
        let mut job = highlight(buf.as_str(), names, error_lines, playing_line);
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    // Apply a pending jump before the widget reads its state.
    if let Some(line) = app.script.jump_to_line.take()
        && let Some((start, _)) = line_bounds(&app.cue_text, line) {
            let char_idx = app.cue_text[..start].chars().count();
            let mut state = egui::text_edit::TextEditState::load(ctx, app.script.id).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(char_idx))));
            state.store(ctx, app.script.id);
            ctx.memory_mut(|m| m.request_focus(app.script.id));
        }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut app.cue_text)
                .id(app.script.id)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(30)
                .lock_focus(true)
                .layouter(&mut layouter),
        );
    });
}

/// What to do to the text after the rows are drawn.
enum RowEdit {
    Set(Row, Option<f64>, String),
    Delete(usize),
    InsertAfter(usize, Option<f64>),
}

fn lines_view(app: &mut VgApp, ui: &mut egui::Ui, ctx: &egui::Context, names: &HashSet<String>, error_lines: &HashSet<usize>, playing_line: Option<usize>) {
    let rows = parse_lines(&app.cue_text);
    let starts: std::collections::HashMap<usize, (f64, f64)> = app.parsed.timeline.as_ref().map(|tl| tl.spans.iter().map(|s| (s.line_no, (s.start, s.end))).collect()).unwrap_or_default();
    let jump = app.script.jump_to_line.take();
    let mut edit: Option<RowEdit> = None;
    let mut seek = None;
    let mut focused = None;
    const TIME_W: f32 = 74.0;
    egui::ScrollArea::vertical().id_salt("script-lines").auto_shrink([false, false]).show(ui, |ui| {
        if rows.is_empty() {
            ui.add_space(12.0);
            ui.label(egui::RichText::new("No lines yet. Transcribe the narration, or add a line.").color(theme::MUTED));
        }
        for row in &rows {
            let now = playing_line == Some(row.line_no);
            let bad = error_lines.contains(&row.line_no);
            let (est, end) = starts.get(&row.line_no).copied().unwrap_or((row.time.unwrap_or(0.0), row.time.unwrap_or(0.0)));
            let top = ui.cursor().top();
            let resp = ui.horizontal_top(|ui| {
                // Timecode: explicit times in white, estimated ones muted. Drag or type to set.
                ui.vertical(|ui| {
                    ui.set_width(TIME_W);
                    let mut t = row.time.unwrap_or(est);
                    let color = if row.time.is_some() { if now { theme::WHITE } else { theme::TEXT2 } } else { theme::MUTED };
                    let dv = ui.add(
                        egui::DragValue::new(&mut t)
                            .speed(0.01)
                            .range(0.0..=36_000.0)
                            .custom_formatter(|v, _| format_time(v))
                            .custom_parser(|s| {
                                let mut v = 0.0;
                                for p in s.trim().split(':') {
                                    v = v * 60.0 + p.trim().parse::<f64>().ok()?;
                                }
                                Some(v)
                            }),
                    );
                    ui.painter().rect_filled(dv.rect, 0.0, theme::BLACK);
                    ui.painter().text(dv.rect.left_center() + egui::vec2(2.0, 0.0), egui::Align2::LEFT_CENTER, if row.time.is_some() { format_time(t) } else { format!("~{}", format_time(t)) }, egui::FontId::monospace(11.5), if dv.hovered() || dv.dragged() { theme::WHITE } else { color });
                    if dv.changed() {
                        edit = Some(RowEdit::Set(row.clone(), Some((t * 100.0).round() / 100.0), row.body.clone()));
                    }
                    if dv.clicked() {
                        seek = Some(row.time.unwrap_or(est));
                    }
                    dv.context_menu(|ui| {
                        if ui.button("Play from here").clicked() {
                            seek = Some(row.time.unwrap_or(est));
                            ui.close();
                        }
                        if row.time.is_some() && ui.button("Let the time follow the words").clicked() {
                            edit = Some(RowEdit::Set(row.clone(), None, row.body.clone()));
                            ui.close();
                        }
                        if row.time.is_none() && ui.button("Pin this time").clicked() {
                            edit = Some(RowEdit::Set(row.clone(), Some((est * 100.0).round() / 100.0), row.body.clone()));
                            ui.close();
                        }
                        if ui.button("Add a line below").clicked() {
                            edit = Some(RowEdit::InsertAfter(row.line_no, None));
                            ui.close();
                        }
                        if ui.button("Delete line").clicked() {
                            edit = Some(RowEdit::Delete(row.line_no));
                            ui.close();
                        }
                    });
                    if end > est {
                        ui.label(egui::RichText::new(format!("{:.1} s", end - est)).monospace().size(9.5).color(theme::MUTED));
                    }
                });
                // The words, with pose tags lit; Enter starts a new line.
                let mut body = row.body.clone();
                let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| -> Arc<egui::Galley> {
                    let mut job = highlight_body(buf.as_str(), names, now, bad);
                    job.wrap.max_width = wrap_width;
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                let te = ui.add(egui::TextEdit::multiline(&mut body).id(row_id(row.line_no)).frame(egui::Frame::NONE).desired_rows(1).desired_width(f32::INFINITY).layouter(&mut layouter));
                if te.has_focus() {
                    focused = Some(row.line_no);
                }
                if te.changed() {
                    edit = Some(RowEdit::Set(row.clone(), row.time, body));
                }
            });
            let r = egui::Rect::from_min_max(egui::pos2(ui.min_rect().left(), top - 2.0), egui::pos2(ui.max_rect().right(), resp.response.rect.bottom() + 2.0));
            // The line being spoken: a white bar at the left edge.
            if now {
                ui.painter().line_segment([r.left_top() + egui::vec2(-4.0, 0.0), r.left_bottom() + egui::vec2(-4.0, 0.0)], egui::Stroke::new(2.0_f32, theme::WHITE));
            }
            if bad {
                theme::dashed_rect(ui.painter(), r, egui::Stroke::new(1.0_f32, theme::BORDER));
            }
            if jump == Some(row.line_no) {
                ui.scroll_to_rect(r, Some(egui::Align::Center));
                ctx.memory_mut(|m| m.request_focus(row_id(row.line_no)));
            }
            ui.add_space(2.0);
            ui.painter().line_segment([egui::pos2(r.left(), r.bottom() + 2.0), egui::pos2(r.right(), r.bottom() + 2.0)], egui::Stroke::new(1.0_f32, theme::RULE_SOFT));
            ui.add_space(3.0);
        }
        ui.add_space(6.0);
        if theme::tool_button(ui, "Add line", None, false).clicked() {
            let last = rows.last().map(|r| r.line_no).unwrap_or(app.cue_text.lines().count());
            edit = Some(RowEdit::InsertAfter(last, Some(app.time)));
        }
        ui.label(egui::RichText::new("Click a time to play from it; drag it to retime the line; right-click for more. Enter splits a line.").size(10.5).color(theme::MUTED));
    });
    app.script.focused = focused.or(app.script.focused).filter(|l| rows.iter().any(|r| r.line_no == *l));
    if let Some(t) = seek {
        app.seek(t);
    }
    match edit {
        Some(RowEdit::Set(row, time, body)) => {
            set_line(&mut app.cue_text, &row, time, &body);
            if body.contains('\n') {
                // Carry on typing in the new line.
                let next = row.line_no + 1;
                let mut st = egui::text_edit::TextEditState::default();
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(0))));
                st.store(ctx, row_id(next));
                ctx.memory_mut(|m| m.request_focus(row_id(next)));
            }
        }
        Some(RowEdit::Delete(line_no)) => delete_line(&mut app.cue_text, line_no),
        Some(RowEdit::InsertAfter(line_no, t)) => {
            let n = insert_line_after(&mut app.cue_text, line_no, t);
            ctx.memory_mut(|m| m.request_focus(row_id(n)));
        }
        None => {}
    }
}

/// The footer: the script's details (directives), each changeable where it can be.
fn footer(app: &mut VgApp, ui: &mut egui::Ui) {
    let d = details(&app.cue_text);
    let get = |k: &str| d.directives.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment([r.left_top(), r.right_top()], egui::Stroke::new(1.0_f32, theme::RULE_SOFT));
    ui.add_space(4.0);
    let file = |v: &Option<String>| v.as_ref().map(|p| std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.clone())).unwrap_or_else(|| "none".into());
    egui::Grid::new("script-details-grid").num_columns(3).spacing([10.0, 4.0]).show(ui, |ui| {
        let (audio, words) = (get("audio"), get("words"));
        ui.label(theme::caption("Narration"));
        ui.add(egui::Label::new(egui::RichText::new(file(&audio)).monospace().size(11.0).color(if audio.is_some() { theme::TEXT2 } else { theme::MUTED })).truncate());
        if theme::tool_button(ui, "Change", None, false).clicked() {
            app.import_audio();
        }
        ui.end_row();
        ui.label(theme::caption("Word timings"));
        ui.add(egui::Label::new(egui::RichText::new(file(&words)).monospace().size(11.0).color(if words.is_some() { theme::TEXT2 } else { theme::MUTED })).truncate());
        ui.horizontal(|ui| {
            if theme::tool_button(ui, "Change", None, false).clicked() {
                let d = rfd::FileDialog::new().add_filter("word timings", &["json"]).set_directory(app.cue_dir());
                super::dialog::open(app, super::dialog::Want::WordsFile, d, super::dialog::How::File);
            }
            if words.is_some() && theme::tool_button(ui, "Clear", None, false).clicked() {
                super::remove_directive(&mut app.cue_text, "words");
                app.reparse();
            }
        });
        ui.end_row();
        ui.label(theme::caption("Start pose"));
        let start = get("start");
        let current = start.clone().unwrap_or_else(|| "first tag".into());
        let mut pick: Option<Option<String>> = None;
        egui::ComboBox::from_id_salt("start-pose").width(150.0).selected_text(current).show_ui(ui, |ui| {
            if ui.selectable_label(start.is_none(), "first tag").clicked() {
                pick = Some(None);
            }
            for p in &app.poses.poses {
                let name = p.short_name().to_string();
                if ui.selectable_label(start.as_deref() == Some(name.as_str()), &name).clicked() {
                    pick = Some(Some(name));
                }
            }
        });
        ui.label("");
        if let Some(choice) = pick {
            match choice {
                Some(n) => super::set_directive(&mut app.cue_text, "start", &n),
                None => super::remove_directive(&mut app.cue_text, "start"),
            }
            app.reparse();
        }
        ui.end_row();
        ui.label(theme::caption("Length"));
        let fixed = get("duration").is_some();
        ui.label(egui::RichText::new(format!("{}{}", format_time(app.duration()), if fixed { " · set" } else { " · the narration's" })).monospace().size(11.0).color(theme::TEXT2));
        if fixed && app.audio.is_some() && theme::tool_button(ui, "Fit to narration", None, false).clicked() {
            super::remove_directive(&mut app.cue_text, "duration");
            app.reparse();
        } else {
            ui.label("");
        }
        ui.end_row();
    });
    let others: Vec<String> = d.directives.iter().filter(|(k, _)| !matches!(k.as_str(), "audio" | "words" | "start" | "duration")).map(|(k, v)| format!("@{k} {v}")).collect();
    let mut notes = vec![];
    if d.comments > 0 {
        notes.push(format!("{} comment line{} hidden", d.comments, plural(d.comments)));
    }
    notes.extend(others);
    if !notes.is_empty() && !app.prefs.script_source {
        ui.label(egui::RichText::new(format!("{} · Source shows everything", notes.join(" · "))).size(10.5).color(theme::MUTED));
    }
    ui.label(egui::RichText::new("Length is set on the timeline (drag END).").size(10.5).color(theme::MUTED));
}

/// What a `[bracketed]` token in a script line is.
#[derive(Clone, Copy)]
enum Token {
    /// The line's leading `[time]`.
    Time,
    /// `[hide]` or `[show]`.
    Special,
    Pose,
    Unknown,
    /// A `[` with no `]`: the rest of the line.
    Unclosed,
}

/// Walk `line` as plain text (`f(text, None)`) and `[tokens]` (`f(token,
/// Some(kind))`). A leading `[time]` is [`Token::Time`] only with `lead_time`.
fn tokens(line: &str, names: &HashSet<String>, lead_time: bool, mut f: impl FnMut(&str, Option<Token>)) {
    let mut rest = line;
    let mut first = lead_time;
    while !rest.is_empty() {
        let Some(open) = rest.find('[') else {
            f(rest, None);
            break;
        };
        if open > 0 {
            f(&rest[..open], None);
        }
        let Some(close) = rest[open..].find(']') else {
            f(&rest[open..], Some(Token::Unclosed));
            break;
        };
        let token = &rest[open..open + close + 1];
        let inner = &token[1..token.len() - 1];
        let name = inner.split('@').next().unwrap_or("").trim().to_lowercase();
        let kind = if first && rest[..open].trim().is_empty() && vg_core::parse_time(inner).is_some() {
            Token::Time
        } else if name == "hide" || name == "show" {
            Token::Special
        } else if names.contains(&name) {
            Token::Pose
        } else {
            Token::Unknown
        };
        f(token, Some(kind));
        rest = &rest[open + close + 1..];
        first = false;
    }
}

/// A line's words with its tags lit (the line list): poses amber, hide/show
/// white, unknown underlined.
fn highlight_body(text: &str, names: &HashSet<String>, now: bool, bad_line: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let base = TextFormat { font_id: egui::FontId::proportional(13.0), color: if now { theme::WHITE } else { theme::TEXT2 }, ..Default::default() };
    let mono = egui::FontId::monospace(12.0);
    tokens(text, names, false, |piece, kind| {
        let f = match kind {
            None => base.clone(),
            Some(Token::Time) => TextFormat { color: theme::MUTED, ..base.clone() },
            Some(Token::Special) => TextFormat { color: theme::WHITE, font_id: mono.clone(), ..base.clone() },
            Some(Token::Pose) => TextFormat { color: theme::MARK, font_id: mono.clone(), ..base.clone() },
            Some(Token::Unknown | Token::Unclosed) => TextFormat { color: theme::WHITE, underline: egui::Stroke::new(1.0_f32, theme::WHITE), ..base.clone() },
        };
        job.append(piece, 0.0, f);
    });
    if bad_line {
        for s in &mut job.sections {
            s.format.underline = egui::Stroke::new(1.0_f32, theme::MUTED);
        }
    }
    job
}

/// The whole cue sheet (the Source view): comments and directives quiet, tags lit.
fn highlight(text: &str, names: &HashSet<String>, error_lines: &HashSet<usize>, playing_line: Option<usize>) -> LayoutJob {
    let mut job = LayoutJob::default();
    let fmt = |color: egui::Color32| TextFormat { font_id: egui::FontId::monospace(12.5), color, ..Default::default() };
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let line_no = i + 1;
        let mut base = fmt(theme::TEXT2);
        if error_lines.contains(&line_no) {
            base.underline = egui::Stroke::new(1.0_f32, theme::WHITE);
        }
        // What is being spoken now is white; everything else stays secondary.
        if playing_line == Some(line_no) {
            base.color = theme::WHITE;
            base.background = egui::Color32::from_gray(0x14);
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            job.append(line, 0.0, fmt(theme::MUTED));
            continue;
        }
        if trimmed.starts_with('@') {
            job.append(line, 0.0, fmt(theme::FOCUS));
            continue;
        }
        tokens(line, names, true, |piece, kind| {
            let f = match kind {
                None => base.clone(),
                Some(Token::Time) => TextFormat { color: theme::MUTED, ..base.clone() },
                Some(Token::Special | Token::Unclosed) => TextFormat { color: theme::WHITE, ..base.clone() },
                Some(Token::Pose) => TextFormat { color: theme::MARK, ..base.clone() },
                Some(Token::Unknown) => TextFormat { color: theme::WHITE, underline: egui::Stroke::new(1.0_f32, theme::WHITE), ..base.clone() },
            };
            job.append(piece, 0.0, f);
        });
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_list_reads_and_rewrites_lines() {
        let src = "# note\n@audio a.wav\n\n[0:01] Hey [wave] there\nno time here\n";
        let rows = parse_lines(src);
        assert_eq!(rows, vec![Row { line_no: 4, time: Some(1.0), body: "Hey [wave] there".into() }, Row { line_no: 5, time: None, body: "no time here".into() }]);
        let d = details(src);
        assert_eq!((d.comments, d.directives.clone()), (1, vec![("audio".to_string(), "a.wav".to_string())]));
        let mut t = src.to_string();
        set_line(&mut t, &rows[0], Some(1.5), "Hey [wave] you\nand more");
        assert_eq!(t, "# note\n@audio a.wav\n\n[0:01.50] Hey [wave] you\nand more\nno time here\n");
        let rows = parse_lines(&t);
        set_line(&mut t, &rows[2], None, "untimed still");
        delete_line(&mut t, 5);
        assert_eq!(t, "# note\n@audio a.wav\n\n[0:01.50] Hey [wave] you\nuntimed still\n");
        assert_eq!(insert_line_after(&mut t, 4, Some(3.0)), 5);
        assert!(t.contains("you\n[0:03.00] \nuntimed"));
    }

    #[test]
    fn timed_tag_goes_on_the_line() {
        let mut t = "a\n[0:01] hello\nc\n".to_string();
        insert_timed_tag(&mut t, Some(2), "wave", 1.5);
        assert_eq!(t, "a\n[0:01] hello [wave@0:01.50]\nc\n");
        insert_timed_tag(&mut t, None, "shrug", 3.0);
        assert_eq!(t, "a\n[0:01] hello [wave@0:01.50]\nc\n[0:03.00] [shrug]\n");
    }

    #[test]
    fn retime_and_delete() {
        let mut t = "x\n[1] hi [wave] there\n".to_string();
        let sheet = vg_core::CueSheet::parse(&t).unwrap();
        let vg_core::Item::Tag(tag) = &sheet.lines[1].items[1] else { panic!() };
        assert!(retime_tag(&mut t, 2, tag.range, "wave", 2.25));
        assert_eq!(t, "x\n[1] hi [wave@0:02.25] there\n");
        let sheet = vg_core::CueSheet::parse(&t).unwrap();
        let vg_core::Item::Tag(tag) = &sheet.lines[1].items[1] else { panic!() };
        assert!(delete_tag(&mut t, 2, tag.range));
        assert_eq!(t, "x\n[1] hi there\n");
    }
}
