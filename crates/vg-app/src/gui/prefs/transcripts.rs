//! The transcripts page: every `*.words.json` in the project's folders, to
//! view, edit word by word, rename, delete, turn into scripts, and edit many at
//! once (find & replace, remove fillers, shift, language) with the scripts that
//! use them kept in step. Also the size limit that cleans up the oldest.

use super::super::{VgApp, theme};
use super::super::theme::weak;
use super::super::util::{ago, file_name, human_bytes, plural, short};
use super::{Action, Confirm, heading};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use vg_core::paths::{canon, relative_path};
use vg_core::{Transcript, Word};
use vg_project::transcripts::{self as tr, Entry, MatchOpts};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Pane {
    #[default]
    Edit,
    Bulk,
    Limit,
}

/// A file's path and its bytes, to put back on undo.
type Backup = (PathBuf, Vec<u8>);

pub struct State {
    pub rescan: bool,
    /// Check the size limit (at start-up and after each transcription).
    pub check_limit: bool,
    entries: Vec<Entry>,
    /// Script texts, for previews: path → text.
    scripts: HashMap<PathBuf, String>,
    ticked: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    filter: String,
    pane: Pane,
    edit: Option<WordEdit>,
    find: String,
    with: String,
    opts: MatchOpts,
    scripts_too: bool,
    shift: f64,
    language: String,
    /// The last bulk edit (its name) and the files it changed, as they were before it.
    undo: Option<(String, Vec<Backup>)>,
}

impl Default for State {
    fn default() -> Self {
        State {
            rescan: true,
            check_limit: true,
            entries: vec![],
            scripts: HashMap::new(),
            ticked: HashSet::new(),
            selected: None,
            filter: String::new(),
            pane: Pane::Edit,
            edit: None,
            find: String::new(),
            with: String::new(),
            opts: MatchOpts::default(),
            scripts_too: true,
            shift: 0.0,
            language: String::new(),
            undo: None,
        }
    }
}

/// One transcript's words being edited.
struct WordEdit {
    path: PathBuf,
    words: Vec<Word>,
    dirty: bool,
    name: String,
}

/// Where transcripts are looked for.
pub fn dirs(app: &VgApp) -> Vec<PathBuf> {
    let mut d = vec![app.base(), app.cue_dir()];
    d.extend(std::env::current_dir().ok());
    d.extend(app.prefs.transcript_dirs.iter().cloned());
    let mut out: Vec<PathBuf> = vec![];
    for p in d {
        let c = canon(&p);
        if !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

fn rescan(app: &mut VgApp) {
    let entries = tr::scan(&dirs(app));
    let st = &mut app.prefs_win.transcripts;
    st.entries = entries;
    st.scripts = st.entries.iter().flat_map(|e| e.scripts.iter()).filter_map(|s| Some((s.clone(), std::fs::read_to_string(s).ok()?))).collect();
    let paths: HashSet<PathBuf> = st.entries.iter().map(|e| e.path.clone()).collect();
    st.ticked.retain(|p| paths.contains(p));
    if st.selected.as_ref().is_some_and(|p| !paths.contains(p)) {
        st.selected = None;
    }
    st.rescan = false;
}

/// The words file the open script uses.
fn open_words(app: &VgApp) -> Option<PathBuf> {
    app.words.as_ref().map(|w| canon(&w.0))
}

/// Whether the size limit must leave a transcript alone.
fn protected(app: &VgApp, e: &Entry) -> bool {
    let c = canon(&e.path);
    app.prefs.transcript_keep.iter().any(|k| canon(k) == c) || open_words(app).as_ref() == Some(&c) || (app.prefs.transcript_limit.keep_used && !e.scripts.is_empty())
}

/// Runs every frame, window open or not: the size limit.
pub fn background(app: &mut VgApp) {
    if !app.prefs_win.transcripts.check_limit {
        return;
    }
    app.prefs_win.transcripts.check_limit = false;
    if !app.prefs.transcript_limit.on {
        return;
    }
    let entries = tr::scan(&dirs(app));
    let (n, bytes) = enforce_limit(app, &entries);
    if n > 0 {
        app.info(format!("transcripts were over the {} MB limit: moved the {n} oldest ({}) to the trash", app.prefs.transcript_limit.mb, human_bytes(bytes)));
        app.prefs_win.transcripts.rescan = true;
    }
}

/// Move the oldest unprotected transcripts (and their recordings, if they
/// count) to the trash until the rest fit. Returns how many and how much.
fn enforce_limit(app: &mut VgApp, entries: &[Entry]) -> (usize, u64) {
    let lim = app.prefs.transcript_limit.clone();
    let limit = (lim.mb * 1e6) as u64;
    let gone: Vec<Entry> = tr::over_limit(entries, limit, lim.with_audio, &|e| protected(app, e)).into_iter().cloned().collect();
    let mut files = vec![];
    let mut bytes = 0;
    for e in &gone {
        files.push(e.path.clone());
        bytes += e.bytes;
        if lim.with_audio
            && let Some(a) = &e.audio {
                // Not the narration that's open.
                if app.audio.as_ref().is_none_or(|o| canon(&o.path) != canon(a)) {
                    files.push(a.clone());
                    bytes += e.audio_bytes;
                }
            }
    }
    if files.is_empty() {
        return (0, 0);
    }
    match trash::delete_all(&files) {
        Ok(()) => (gone.len(), bytes),
        Err(e) => {
            app.error(format!("transcript limit: moving to the trash: {e}"));
            (0, 0)
        }
    }
}

pub fn show(app: &mut VgApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    if app.prefs_win.transcripts.rescan {
        rescan(app);
    }
    folders(app, ui);
    list(app, ui);
    ui.add_space(6.0);
    ui.separator();
    let n = app.prefs_win.transcripts.ticked.len();
    let label = format!("Bulk edit · {n} ticked");
    theme::segmented(ui, &mut app.prefs_win.transcripts.pane, &[(Pane::Edit, "Edit selected"), (Pane::Bulk, label.as_str()), (Pane::Limit, "Storage limit")]);
    ui.add_space(4.0);
    egui::ScrollArea::vertical().id_salt("tr-pane").auto_shrink([false, false]).show(ui, |ui| match app.prefs_win.transcripts.pane {
        Pane::Edit => edit_pane(app, ui, ctx),
        Pane::Bulk => bulk_pane(app, ui),
        Pane::Limit => limit_pane(app, ui),
    });
}

fn folders(app: &mut VgApp, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.label(theme::caption("Folders"));
        let extra: Vec<PathBuf> = app.prefs.transcript_dirs.iter().map(|d| canon(d)).collect();
        let mut remove = None;
        for (k, d) in dirs(app).into_iter().enumerate() {
            if k > 0 {
                ui.label(egui::RichText::new("·").color(theme::MUTED));
            }
            ui.label(egui::RichText::new(short(&d)).monospace().size(11.0).color(theme::TEXT2));
            if let Some(k) = extra.iter().position(|e| *e == d)
                && ui.small_button("×").clicked() {
                    remove = Some(k);
                }
        }
        if let Some(k) = remove {
            app.prefs.transcript_dirs.remove(k);
            app.prefs_win.transcripts.rescan = true;
        }
        if theme::tool_button(ui, "Add folder", None, false).clicked() {
            let d = rfd::FileDialog::new().set_directory(app.base());
            super::super::dialog::open(app, super::super::dialog::Want::TranscriptDir, d, super::super::dialog::How::Folder);
        }
    });
    weak(ui, "Transcripts are the <recording>.words.json files Transcribe writes: the words Whisper heard and when. Scripts point at one with @words; captions take their timing from it.");
}

fn list(app: &mut VgApp, ui: &mut egui::Ui) {
    let open = open_words(app);
    ui.horizontal(|ui| {
        let st = &mut app.prefs_win.transcripts;
        let total: u64 = st.entries.iter().map(|e| e.bytes).sum();
        heading(ui, &format!("Transcripts · {} · {}", st.entries.len(), human_bytes(total)));
        ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("filter by name or words").desired_width(180.0));
        if theme::tool_button(ui, "All", None, false).clicked() {
            let f = st.filter.to_lowercase();
            st.ticked = st.entries.iter().filter(|e| matches(e, &f)).map(|e| e.path.clone()).collect();
        }
        if theme::tool_button(ui, "None", None, false).clicked() {
            st.ticked.clear();
        }
        if theme::tool_button(ui, "Refresh", None, false).clicked() {
            st.rescan = true;
        }
        if theme::tool_button(ui, "Transcribe audio", None, false).clicked() {
            let d = rfd::FileDialog::new().add_filter("audio", vg_project::transcripts::AUDIO_EXTS).set_directory(app.cue_dir());
            super::super::dialog::open(app, super::super::dialog::Want::TranscribeAudio { open: true }, d, super::super::dialog::How::File);
        }
        let n = app.prefs_win.transcripts.ticked.len();
        if theme::tool_button_enabled(ui, &format!("Delete {n} transcript{}", plural(n)), None, false, n > 0).clicked() {
            delete(app);
        }
    });
    let st = &mut app.prefs_win.transcripts;
    let f = st.filter.to_lowercase();
    let keep: Vec<PathBuf> = app.prefs.transcript_keep.iter().map(|k| canon(k)).collect();
    let mut pin: Option<PathBuf> = None;
    let mut select: Option<PathBuf> = None;
    egui::ScrollArea::vertical().id_salt("tr-list").max_height((ui.available_height() * 0.42).max(140.0)).auto_shrink([false, true]).show(ui, |ui| {
        egui::Grid::new("transcripts").num_columns(9).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
            for h in ["", "Name", "Words", "Length", "Lang", "Recording", "Used by", "Size", "Changed"] {
                ui.label(theme::caption(h));
            }
            ui.end_row();
            for e in st.entries.iter().filter(|e| matches(e, &f)) {
                let mut on = st.ticked.contains(&e.path);
                if ui.checkbox(&mut on, "").changed() {
                    if on {
                        st.ticked.insert(e.path.clone());
                    } else {
                        st.ticked.remove(&e.path);
                    }
                }
                let c = canon(&e.path);
                let sel = st.selected.as_ref() == Some(&e.path);
                let is_open = open.as_ref() == Some(&c);
                let pinned = keep.contains(&c);
                ui.horizontal(|ui| {
                    let name = egui::RichText::new(e.name()).color(if sel { theme::BLACK } else if is_open { theme::MARK } else { theme::WHITE });
                    let r = ui.add(egui::Button::new(if sel { name.background_color(theme::WHITE) } else { name }).frame(false));
                    if r.clicked() {
                        select = Some(e.path.clone());
                    }
                    if ui.add(egui::Button::new(egui::RichText::new(if pinned { "kept" } else { "keep" }).size(10.0).color(if pinned { theme::WHITE } else { theme::MUTED })).frame(false)).clicked() {
                        pin = Some(c.clone());
                    }
                });
                if let Some(err) = &e.error {
                    ui.label(egui::RichText::new("unreadable").size(11.0).color(theme::TEXT2)).on_hover_text(err);
                } else {
                    ui.label(egui::RichText::new(e.word_count().to_string()).size(11.0));
                }
                ui.label(egui::RichText::new(vg_core::format_time(e.transcript.duration.max(e.transcript.segments.last().map_or(0.0, |s| s.end)))).size(11.0).color(theme::TEXT2));
                ui.label(egui::RichText::new(if e.transcript.language.is_empty() { "—" } else { &e.transcript.language }).size(11.0).color(theme::TEXT2));
                ui.label(egui::RichText::new(e.audio.as_ref().map(|_| human_bytes(e.audio_bytes)).unwrap_or_else(|| "missing".into())).size(11.0).color(if e.audio.is_some() { theme::TEXT2 } else { theme::MUTED }));
                let used = match e.scripts.len() {
                    0 => "—".to_string(),
                    1 => file_name(&e.scripts[0]),
                    n => format!("{n} scripts"),
                };
                ui.label(egui::RichText::new(if is_open { format!("{used} (open)") } else { used }).size(11.0).color(if is_open { theme::MARK } else { theme::TEXT2 }));
                theme::note(ui, human_bytes(e.bytes));
                theme::note(ui, ago(e.modified));
                ui.end_row();
            }
        });
    });
    if let Some(p) = select {
        st.selected = Some(p);
        st.pane = Pane::Edit;
        st.edit = None;
    }
    if let Some(c) = pin {
        if let Some(k) = app.prefs.transcript_keep.iter().position(|k| canon(k) == c) {
            app.prefs.transcript_keep.remove(k);
        } else {
            app.prefs.transcript_keep.push(c);
        }
    }
}

fn matches(e: &Entry, f: &str) -> bool {
    f.is_empty() || e.name().to_lowercase().contains(f) || e.text().to_lowercase().contains(f)
}

fn delete(app: &mut VgApp) {
    let st = &app.prefs_win.transcripts;
    let chosen: Vec<&Entry> = st.entries.iter().filter(|e| st.ticked.contains(&e.path)).collect();
    let mut files: Vec<(PathBuf, bool)> = vec![];
    let mut used = vec![];
    for e in &chosen {
        files.push((e.path.clone(), true));
        if let Some(a) = &e.audio {
            files.push((a.clone(), false));
        }
        used.extend(e.scripts.iter().map(|s| file_name(s)));
    }
    let body = if used.is_empty() {
        "The ticked files go to the trash. Recordings aren't ticked; tick them to remove those too.".to_string()
    } else {
        format!("The ticked files go to the trash. {} still point at them: their captions lose the word timings (they're spread by length instead). Recordings aren't ticked; tick them to remove those too.", used.join(", "))
    };
    app.prefs_win.confirm = Some(Confirm { title: format!("Delete {} transcript{}", chosen.len(), plural(chosen.len())), body, files, ok: "Move to trash".into(), action: Action::Trash { closes_project: false } });
}

// ---- one transcript ---------------------------------------------------------------

fn edit_pane(app: &mut VgApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    let Some(path) = app.prefs_win.transcripts.selected.clone() else {
        weak(ui, "Click a transcript's name to see and edit it here.");
        return;
    };
    let Some(e) = app.prefs_win.transcripts.entries.iter().find(|e| e.path == path).cloned() else { return };
    if app.prefs_win.transcripts.edit.as_ref().is_none_or(|w| w.path != path) {
        app.prefs_win.transcripts.edit = Some(WordEdit { path: path.clone(), words: e.transcript.words(), dirty: false, name: e.name() });
    }
    let is_open = open_words(app).as_ref() == Some(&canon(&path));

    ui.horizontal(|ui| {
        let w = app.prefs_win.transcripts.edit.as_mut().unwrap();
        ui.label(theme::caption("Name"));
        ui.add(egui::TextEdit::singleline(&mut w.name).desired_width(200.0));
        let new = w.name.trim().to_string();
        if theme::tool_button_enabled(ui, "Rename", None, false, new != e.name() && !new.is_empty()).clicked() {
            rename(app, &e, &new);
        }
        ui.label(egui::RichText::new(short(&path)).monospace().size(11.0).color(theme::MUTED));
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(theme::caption("Recording"));
        ui.label(egui::RichText::new(e.audio.as_deref().map(short).unwrap_or_else(|| "not found next to it".into())).size(11.0).color(theme::TEXT2));
        if let Some(a) = e.audio.clone() {
            if theme::tool_button(ui, "Transcribe again", None, false).clicked() {
                super::super::transcribe::open_for(app, a.clone());
                super::super::transcribe::run_whisper(app, ctx);
            }
            if theme::tool_button(ui, "New script from it", None, false).clicked() {
                new_script(app, ctx, &e, &a);
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(theme::caption("Used by"));
        if e.scripts.is_empty() {
            theme::note(ui, "no script");
        }
        for s in &e.scripts {
            let name = file_name(s);
            if ui.add(egui::Button::new(egui::RichText::new(name).size(11.0)).frame(false)).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                && let Err(err) = app.open_script(s) {
                    app.error(format!("{err:#}"));
                }
        }
    });

    let w = app.prefs_win.transcripts.edit.as_mut().unwrap();
    let mut save = None;
    ui.horizontal(|ui| {
        heading(ui, &format!("Words · {}", w.words.len()));
        if theme::tool_button_enabled(ui, "Save", None, w.dirty, w.dirty).clicked() {
            save = Some(w.words.clone());
        }
        if theme::tool_button_enabled(ui, "Revert", None, false, w.dirty).clicked() {
            w.words = e.transcript.words();
            w.dirty = false;
        }
    });
    if let Some(words) = save {
        save_words(app, &path, words, is_open);
        return;
    }
    let Some(w) = app.prefs_win.transcripts.edit.as_mut() else { return };
    weak(ui, "Captions show the script's words, timed by these. Retime or fix words here; to change a word in the scripts too, use Bulk edit.");
    let row_h = 22.0;
    let mut remove = None;
    egui::ScrollArea::vertical().id_salt("tr-words").max_height(260.0).auto_shrink([false, true]).show_rows(ui, row_h, w.words.len(), |ui, rows| {
        for i in rows {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{:>4}", i + 1)).monospace().size(10.5).color(theme::MUTED));
                let word = &mut w.words[i];
                let a = ui.add(egui::DragValue::new(&mut word.start).speed(0.01).range(0.0..=f64::MAX).fixed_decimals(2).suffix(" s"));
                let b = ui.add(egui::DragValue::new(&mut word.end).speed(0.01).range(word.start..=f64::MAX).fixed_decimals(2).suffix(" s"));
                let c = ui.add(egui::TextEdit::singleline(&mut word.word).desired_width(220.0));
                if a.changed() || b.changed() || c.changed() {
                    w.dirty = true;
                }
                if ui.small_button("×").clicked() {
                    remove = Some(i);
                }
            });
        }
    });
    if let Some(i) = remove {
        w.words.remove(i);
        w.dirty = true;
    }
}

fn save_words(app: &mut VgApp, path: &Path, mut words: Vec<Word>, is_open: bool) {
    if is_open && app.words_dirty {
        app.error("the open script has unsaved word timings; save it (Ctrl+S) first");
        return;
    }
    words.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut t = Transcript::load(path).unwrap_or_default();
    t.segments = vec![vg_core::transcript::Segment {
        start: words.first().map_or(0.0, |w| w.start),
        end: words.last().map_or(0.0, |w| w.end),
        text: words.iter().map(|w| w.word.trim()).collect::<Vec<_>>().join(" "),
        words,
    }];
    let before = std::fs::read(path).unwrap_or_default();
    match t.save(path) {
        Ok(()) => {
            app.prefs_win.transcripts.undo = Some(("word edits".into(), vec![(path.to_path_buf(), before)]));
            app.reparse();
            app.info(format!("saved {}", short(path)));
            app.prefs_win.transcripts.rescan = true;
            if let Some(w) = &mut app.prefs_win.transcripts.edit {
                w.dirty = false;
            }
        }
        Err(e) => app.error(format!("{e:#}")),
    }
}

fn rename(app: &mut VgApp, e: &Entry, new: &str) {
    let to = match tr::renamed_path(&e.path, new) {
        Ok(p) => p,
        Err(err) => return app.error(format!("{err:#}")),
    };
    if to.exists() {
        return app.error(format!("{} already exists", short(&to)));
    }
    let open_script = app.cue_path.as_deref().map(canon);
    if e.scripts.iter().any(|s| Some(canon(s)) == open_script) && app.cue_text != app.cue_saved_text {
        return app.error("the open script uses this transcript and has unsaved changes; save it first");
    }
    let edits = match tr::repoint_scripts(&e.scripts, &e.path, &to) {
        Ok(x) => x,
        Err(err) => return app.error(format!("{err:#}")),
    };
    if let Err(err) = std::fs::rename(&e.path, &to) {
        return app.error(format!("renaming: {err}"));
    }
    for (s, text) in edits {
        if let Err(err) = write_script(app, &s, &text) {
            app.error(format!("{err:#}"));
        }
    }
    for k in &mut app.prefs.transcript_keep {
        if canon(k) == canon(&to) || *k == e.path {
            *k = canon(&to);
        }
    }
    app.prefs_win.transcripts.selected = Some(to.clone());
    app.prefs_win.transcripts.edit = None;
    app.prefs_win.transcripts.undo = None;
    app.prefs_win.transcripts.rescan = true;
    app.reparse();
    app.info(format!("renamed to {} ({} script{} updated)", short(&to), e.scripts.len(), plural(e.scripts.len())));
}

fn new_script(app: &mut VgApp, ctx: &egui::Context, e: &Entry, audio: &Path) {
    let dir = audio.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = e.name();
    let mut path = dir.join(format!("{stem}.md"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.md"));
        n += 1;
    }
    let poses = (app.prefs.transcribe_autotag && !app.prefs.transcribe_llm && !app.poses.poses.is_empty()).then_some(&app.poses);
    let (words_file, _) = vg_project::transcript_paths(audio);
    let rel = |p: &Path| relative_path(p, &dir).to_string_lossy().to_string();
    let text = vg_core::transcript::cue_sheet(&e.transcript, &rel(audio), &rel(&words_file), poses, &format!("Made from {} (transcript).", short(&e.path)));
    if app.cue_text != app.cue_saved_text {
        return app.error("the open script has unsaved changes; save it first");
    }
    if let Err(err) = std::fs::write(&path, &text) {
        return app.error(format!("writing {}: {err}", path.display()));
    }
    if let Err(err) = app.open_script(&path) {
        return app.error(format!("{err:#}"));
    }
    app.info(format!("wrote {}", short(&path)));
    app.prefs_win.transcripts.rescan = true;
    if app.prefs.transcribe_autotag && app.prefs.transcribe_llm {
        super::super::assist::start(app, ctx, vg_llm::Mode::Replace);
    }
}

/// Write a script, keeping the open copy in step.
fn write_script(app: &mut VgApp, path: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::write(path, text)?;
    if app.cue_path.as_deref().map(canon) == Some(canon(path)) {
        app.cue_text = text.to_string();
        app.cue_saved_text = text.to_string();
        app.watch.cue = Some((path.to_path_buf(), super::super::mtime(path)));
    }
    Ok(())
}

// ---- many at once -----------------------------------------------------------------

enum Op {
    Replace,
    Fillers,
    Shift,
    Language,
}

/// The new texts of the ticked transcripts (and their scripts) for `op`, with counts.
fn plan(app: &VgApp, op: &Op) -> (Vec<(PathBuf, String)>, usize, usize) {
    let st = &app.prefs_win.transcripts;
    let fillers: Vec<String> = app.prefs.fillers.split(',').map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
    let mut writes = vec![];
    let (mut in_t, mut in_s) = (0, 0);
    let mut scripts_done: HashSet<PathBuf> = HashSet::new();
    for e in st.entries.iter().filter(|e| st.ticked.contains(&e.path) && e.error.is_none()) {
        let mut t = e.transcript.clone();
        let n = match op {
            Op::Replace => tr::replace(&mut t, &st.find, &st.with, st.opts),
            Op::Fillers => tr::remove(&mut t, &fillers),
            Op::Shift if st.shift != 0.0 => tr::shift(&mut t, st.shift),
            Op::Shift => 0,
            Op::Language if t.language != st.language.trim() => {
                t.language = st.language.trim().to_string();
                1
            }
            Op::Language => 0,
        };
        if n > 0 {
            in_t += n;
            writes.push((e.path.clone(), serde_json::to_string(&t).unwrap_or_default() + "\n"));
        }
        if !st.scripts_too || matches!(op, Op::Language) {
            continue;
        }
        for s in &e.scripts {
            if !scripts_done.insert(s.clone()) {
                continue;
            }
            let Some(text) = st.scripts.get(s) else { continue };
            let (new, k) = match op {
                Op::Replace => tr::replace_in_script(text, &st.find, &st.with, st.opts),
                Op::Fillers => tr::remove_in_script(text, &fillers),
                Op::Shift => match vg_core::cues::remap_times(text, |t| (t + st.shift).max(0.0)) {
                    Ok(new) if new != *text => (new, 1),
                    _ => (text.clone(), 0),
                },
                Op::Language => (text.clone(), 0),
            };
            if k > 0 {
                in_s += k;
                writes.push((s.clone(), new));
            }
        }
    }
    (writes, in_t, in_s)
}

fn run(app: &mut VgApp, op: Op, label: &str) {
    let (writes, _, _) = plan(app, &op);
    if writes.is_empty() {
        return app.info("nothing to change");
    }
    let open_script = app.cue_path.as_deref().map(canon);
    let open_w = open_words(app);
    for (p, _) in &writes {
        let c = canon(p);
        if Some(&c) == open_script.as_ref() && app.cue_text != app.cue_saved_text {
            return app.error("the open script would change but has unsaved edits; save it (Ctrl+S) first");
        }
        if Some(&c) == open_w.as_ref() && app.words_dirty {
            return app.error("the open script's word timings have unsaved edits; save them (Ctrl+S) first");
        }
    }
    let mut before = vec![];
    for (p, text) in &writes {
        before.push((p.clone(), std::fs::read(p).unwrap_or_default()));
        let r = if p.to_string_lossy().ends_with(".md") { write_script(app, p, text) } else { std::fs::write(p, text).map_err(Into::into) };
        if let Err(e) = r {
            app.error(format!("writing {}: {e:#}", p.display()));
            break;
        }
    }
    let n = before.len();
    app.prefs_win.transcripts.undo = Some((label.to_string(), before));
    app.prefs_win.transcripts.rescan = true;
    app.prefs_win.transcripts.edit = None;
    app.reparse();
    app.info(format!("{label}: changed {n} file{}", plural(n)));
}

fn undo(app: &mut VgApp) {
    let Some((label, files)) = app.prefs_win.transcripts.undo.take() else { return };
    for (p, bytes) in &files {
        let r = if p.to_string_lossy().ends_with(".md") { write_script(app, p, &String::from_utf8_lossy(bytes)) } else { std::fs::write(p, bytes).map_err(Into::into) };
        if let Err(e) = r {
            app.error(format!("restoring {}: {e:#}", p.display()));
        }
    }
    app.prefs_win.transcripts.rescan = true;
    app.prefs_win.transcripts.edit = None;
    app.reparse();
    app.info(format!("undid {label}"));
}

fn bulk_pane(app: &mut VgApp, ui: &mut egui::Ui) {
    let n = app.prefs_win.transcripts.ticked.len();
    if n == 0 {
        weak(ui, "Tick transcripts in the list (or All) to edit them together.");
    }
    ui.horizontal(|ui| {
        ui.checkbox(&mut app.prefs_win.transcripts.scripts_too, "also change the scripts that use them");
        if let Some((label, files)) = &app.prefs_win.transcripts.undo {
            let text = format!("Undo {label} ({} file{})", files.len(), plural(files.len()));
            if theme::tool_button(ui, &text, None, false).clicked() {
                undo(app);
            }
        }
    });
    weak(ui, "Changing both keeps each script line's words matched to the transcript's, so the captions stay timed word by word.");
    let counts = |app: &VgApp, op: Op| {
        let (_, t, s) = plan(app, &op);
        if app.prefs_win.transcripts.scripts_too { format!("{t} in transcripts · {s} in scripts") } else { format!("{t} in transcripts") }
    };

    heading(ui, "Find and replace");
    ui.horizontal(|ui| {
        let st = &mut app.prefs_win.transcripts;
        ui.add(egui::TextEdit::singleline(&mut st.find).hint_text("find, e.g. code bullet").desired_width(200.0));
        ui.label("→");
        ui.add(egui::TextEdit::singleline(&mut st.with).hint_text("replace with (empty deletes)").desired_width(200.0));
        ui.checkbox(&mut st.opts.whole_word, "whole words");
        ui.checkbox(&mut st.opts.match_case, "match case");
    });
    ui.horizontal(|ui| {
        let ready = n > 0 && !app.prefs_win.transcripts.find.trim().is_empty();
        if theme::tool_button_enabled(ui, "Replace all", None, false, ready).clicked() {
            run(app, Op::Replace, "replace");
        }
        if ready {
            weak(ui, counts(app, Op::Replace));
        }
    });

    heading(ui, "Remove filler words");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.prefs.fillers).desired_width(360.0));
        if theme::tool_button_enabled(ui, "Remove", None, false, n > 0).clicked() {
            run(app, Op::Fillers, "filler removal");
        }
        if n > 0 {
            weak(ui, counts(app, Op::Fillers));
        }
    });

    heading(ui, "Shift times");
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut app.prefs_win.transcripts.shift).speed(0.01).range(-600.0..=600.0).fixed_decimals(2).suffix(" s"));
        let ready = n > 0 && app.prefs_win.transcripts.shift != 0.0;
        if theme::tool_button_enabled(ui, "Shift", None, false, ready).clicked() {
            run(app, Op::Shift, "time shift");
        }
        weak(ui, "Moves every word (and, with scripts, every line's time), e.g. after trimming the start of a recording.");
    });

    heading(ui, "Language");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.prefs_win.transcripts.language).hint_text("e.g. en").desired_width(80.0));
        let ready = n > 0 && !app.prefs_win.transcripts.language.trim().is_empty();
        if theme::tool_button_enabled(ui, "Set", None, false, ready).clicked() {
            run(app, Op::Language, "language change");
        }
    });
}

// ---- the size limit ---------------------------------------------------------------

fn limit_pane(app: &mut VgApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let l = &mut app.prefs.transcript_limit;
        ui.checkbox(&mut l.on, "Keep transcripts under");
        ui.add(egui::DragValue::new(&mut l.mb).range(1.0..=1_000_000.0).speed(5.0).suffix(" MB"));
    });
    ui.checkbox(&mut app.prefs.transcript_limit.with_audio, "count each transcript's recording too, and remove it with the transcript");
    ui.checkbox(&mut app.prefs.transcript_limit.keep_used, "never remove transcripts a script uses");
    weak(ui, "When they go over, the oldest move to the trash (restore them from there), at start-up and after each transcription. Transcripts marked \"kept\" and the open script's are never removed.");
    let lim = app.prefs.transcript_limit.clone();
    let st = &app.prefs_win.transcripts;
    let used: u64 = st.entries.iter().map(|e| e.bytes + if lim.with_audio { e.audio_bytes } else { 0 }).sum();
    let limit = (lim.mb * 1e6) as u64;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(260.0, 8.0), egui::Sense::hover());
        let frac = (used as f64 / limit.max(1) as f64).min(1.0) as f32;
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Inside);
        ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * frac, rect.height())), 0.0, if used > limit { theme::MARK } else { theme::WHITE });
        ui.label(egui::RichText::new(format!("{} of {}", human_bytes(used), human_bytes(limit))).monospace().size(11.5));
    });
    let gone: Vec<String> = tr::over_limit(&st.entries, limit, lim.with_audio, &|e| protected(app, e)).iter().map(|e| e.name()).collect();
    if gone.is_empty() {
        weak(ui, if used > limit { "Over the limit, but everything left is kept." } else { "Within the limit." });
    } else {
        weak(ui, format!("Over the limit: {} would go, oldest first: {}", gone.len(), gone.join(", ")));
        if theme::tool_button_enabled(ui, "Clean up now", None, false, lim.on).clicked() {
            let entries = app.prefs_win.transcripts.entries.clone();
            let (n, bytes) = enforce_limit(app, &entries);
            app.info(format!("moved {n} transcript{} ({}) to the trash", plural(n), human_bytes(bytes)));
            app.prefs_win.transcripts.rescan = true;
        }
    }
}
