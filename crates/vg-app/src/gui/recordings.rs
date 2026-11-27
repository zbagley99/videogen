//! The recordings browser (Audio tab › Open recording): every recording in the
//! project's folders with its transcript and the scripts that use it, to edit
//! it, add it as a track, make it the narration, or open its script.

use super::util::{AUDIO_EXTS, file_name, mtime};
use super::{VgApp, dialog::Want, place, theme};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use vg_core::paths::canon;


#[derive(Default)]
pub struct Recordings {
    pub open: bool,
    list: Vec<Rec>,
    /// Folders the list was made for (rescan when they change, or on Refresh).
    listed_for: Option<Vec<PathBuf>>,
    filter: String,
}

struct Rec {
    path: PathBuf,
    duration: Option<f64>,
    modified: Option<SystemTime>,
    /// `<stem>.words.json` and its word count.
    transcript: Option<(PathBuf, usize)>,
    /// Scripts whose `@audio` is this recording.
    scripts: Vec<PathBuf>,
}

/// `@audio` of a script, resolved.
fn audio_of(text: &str, dir: &Path) -> Option<PathBuf> {
    text.lines().find_map(|l| {
        let v = l.trim().strip_prefix("@audio ")?.split(" #").next()?.trim();
        (!v.is_empty()).then(|| canon(&vg_project::resolve(dir, Path::new(v))))
    })
}

fn scan(dirs: &[PathBuf]) -> Vec<Rec> {
    let mut audio = vec![];
    let mut scripts: Vec<(PathBuf, PathBuf)> = vec![];
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if AUDIO_EXTS.contains(&ext.as_str()) && p.is_file() {
                audio.push(p);
            } else if ext == "md"
                && let Some(a) = std::fs::read_to_string(&p).ok().and_then(|t| audio_of(&t, d)) {
                    scripts.push((p, a));
                }
        }
    }
    let mut out: Vec<Rec> = audio
        .into_iter()
        .map(|path| {
            let duration = if path.extension().is_some_and(|e| e == "wav") { crate::recorder::wav_duration(&path) } else { vg_media::probe_duration(&path).ok() };
            let (words, _) = vg_project::transcript_paths(&path);
            let transcript = words.exists().then(|| {
                let n = vg_core::Transcript::load(&words).map(|t| t.words().len()).unwrap_or(0);
                (words, n)
            });
            let c = canon(&path);
            let mut used: Vec<PathBuf> = scripts.iter().filter(|(_, a)| *a == c).map(|(s, _)| s.clone()).collect();
            used.sort();
            Rec { modified: mtime(&path), duration, transcript, scripts: used, path }
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.path.cmp(&b.path)));
    out
}

enum Act {
    Edit(PathBuf),
    Track(PathBuf),
    Narration(PathBuf, Option<PathBuf>),
    Script(PathBuf),
    Transcribe(PathBuf),
}

pub fn window(app: &mut VgApp, ctx: &egui::Context) {
    if !app.recordings.open {
        return;
    }
    let dirs = super::prefs::transcripts::dirs(app);
    if app.recordings.listed_for.as_ref() != Some(&dirs) {
        app.recordings.list = scan(&dirs);
        app.recordings.listed_for = Some(dirs);
    }
    let mut open = true;
    let mut act = None;
    let mut browse = None;
    let narration = app.parsed.audio_path.as_deref().map(canon);
    let editing = app.studio.path.as_ref().and(app.studio.loaded_for.as_deref()).map(canon);
    let dir = app.cue_dir();
    let has_edit = app.studio.path.is_some();
    let pos = app.place.place(ctx, "recordings", egui::Align2::LEFT_TOP, egui::vec2(760.0, 460.0));
    place::at(egui::Window::new(theme::caption("Open a recording").color(theme::WHITE)), "recordings", pos)
        .open(&mut open)
        .resizable(true)
        .collapsible(false)
        .default_size([760.0, 440.0])
        .show(ctx, |ui| {
            let r = &mut app.recordings;
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut r.filter).hint_text("filter").desired_width(200.0));
                if theme::tool_button(ui, "Refresh", None, false).clicked() {
                    r.listed_for = None;
                }
                if theme::tool_button(ui, "Browse for a file", None, false).clicked() {
                    browse = Some(Want::RecordingEdit);
                }
                if theme::tool_button(ui, "Add music or effects", None, false).clicked() {
                    browse = Some(Want::RecordingTrack);
                }
            });
            theme::weak(ui, "Edit opens a recording here. Add as track puts it in the open edit at the playhead. Use as narration points the script at it, with its transcript's word timings.");
            ui.add_space(4.0);
            let f = r.filter.to_lowercase();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for rec in r.list.iter().filter(|x| f.is_empty() || file_name(&x.path).to_lowercase().contains(&f)) {
                    let c = canon(&rec.path);
                    let is_narr = narration.as_ref() == Some(&c);
                    let is_edit = editing.as_ref() == Some(&c);
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(egui::RichText::new(file_name(&rec.path)).color(if is_narr { theme::MARK } else { theme::WHITE })).extend());
                            let mut tags = vec![];
                            if is_narr {
                                tags.push("the narration");
                            }
                            if is_edit {
                                tags.push("open here");
                            }
                            if !tags.is_empty() {
                                ui.label(theme::caption(tags.join(" · ")));
                            }
                        });
                        let mut info = vec![rec.duration.map(vg_core::format_time).unwrap_or_else(|| "—".into()), super::util::ago(rec.modified)];
                        info.push(match &rec.transcript {
                            Some((_, n)) => format!("transcript: {n} words"),
                            None => "no transcript".into(),
                        });
                        match rec.scripts.len() {
                            0 => {}
                            1 => info.push(format!("script: {}", file_name(&rec.scripts[0]))),
                            n => info.push(format!("{n} scripts")),
                        }
                        if file_name(&rec.path).ends_with(".mix.wav") {
                            info.push("cleaned-up mix".into());
                        }
                        ui.label(egui::RichText::new(info.join(" · ")).size(10.5).color(theme::MUTED));
                        ui.horizontal(|ui| {
                            if theme::tool_button_enabled(ui, "Edit", None, false, !is_edit).clicked() {
                                act = Some(Act::Edit(rec.path.clone()));
                            }
                            if theme::tool_button_enabled(ui, "Add as track", None, false, !is_edit && has_edit).clicked() {
                                act = Some(Act::Track(rec.path.clone()));
                            }
                            if theme::tool_button_enabled(ui, "Use as narration", None, false, !is_narr).clicked() {
                                act = Some(Act::Narration(rec.path.clone(), rec.transcript.as_ref().map(|t| t.0.clone())));
                            }
                            match rec.scripts.len() {
                                0 if rec.transcript.is_none() => {
                                    if theme::tool_button(ui, "Transcribe", None, false).clicked() {
                                        act = Some(Act::Transcribe(rec.path.clone()));
                                    }
                                }
                                0 => {}
                                1 => {
                                    if theme::tool_button(ui, "Open script", None, false).clicked() {
                                        act = Some(Act::Script(rec.scripts[0].clone()));
                                    }
                                }
                                _ => {
                                    ui.menu_button(egui::RichText::new("Open script").monospace().size(12.0), |ui| {
                                        for s in &rec.scripts {
                                            if ui.button(file_name(s)).clicked() {
                                                act = Some(Act::Script(s.clone()));
                                                ui.close();
                                            }
                                        }
                                    });
                                }
                            }
                        });
                    });
                }
                if r.list.is_empty() {
                    theme::weak(ui, "No recordings in the project's folders yet.");
                }
            });
        });
    app.recordings.open = open;
    if let Some(want) = browse {
        super::dialog::open(app, want, rfd::FileDialog::new().add_filter("audio", AUDIO_EXTS).set_directory(&dir), super::dialog::How::File);
    }
    let Some(act) = act else { return };
    match act {
        Act::Edit(p) => edit(app, &p),
        Act::Track(p) => super::studio::add_track(app, &p, true, None),
        Act::Narration(p, words) => {
            app.set_narration(&p);
            if let Some(w) = words {
                let rel = super::relative_path(&w, &app.cue_dir());
                super::set_directive(&mut app.cue_text, "words", &rel.to_string_lossy());
                app.reparse();
            }
            app.info(format!("narration: {} (Ctrl+S saves the script)", file_name(&p)));
            app.recordings.listed_for = None;
        }
        Act::Script(s) => {
            if app.cue_text != app.cue_saved_text {
                return app.error("the open script has unsaved changes; save it (Ctrl+S) first");
            }
            match app.open_script(&s) {
                Ok(()) => app.info(format!("opened {}", file_name(&s))),
                Err(e) => app.error(format!("{e:#}")),
            }
        }
        Act::Transcribe(p) => {
            super::transcribe::open_for(app, p);
        }
    }
}

/// Open a recording in the Audio tab (saving the open edit first).
pub(super) fn edit(app: &mut VgApp, p: &Path) {
    if app.studio.dirty()
        && let Err(e) = app.studio.save() {
            return app.error(format!("{e:#}"));
        }
    match app.studio.open_for(p) {
        Ok(()) => {
            app.studio.manual = Some(app.parsed.audio_path.clone());
            app.info(format!("editing {}", file_name(p)));
        }
        Err(e) => app.error(format!("{e:#}")),
    }
}
