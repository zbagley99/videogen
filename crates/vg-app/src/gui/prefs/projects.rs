//! The projects page: recent projects and those in the projects folder, with
//! open, new, rename, duplicate, forget and delete (to the trash).

use super::super::{VgApp, theme};
use super::super::theme::weak;
use super::super::util::{ago, file_name, short};
use super::{Action, Confirm, heading};
use eframe::egui;
use std::path::{Path, PathBuf};
use vg_core::paths::canon;
use vg_project::projects::{self, Info};

pub struct State {
    pub rescan: bool,
    list: Vec<(Info, bool)>,
    new_name: String,
    /// A row being renamed or duplicated: (project, new name, also rename the script).
    rename: Option<(PathBuf, String, bool)>,
    duplicate: Option<(PathBuf, String)>,
    filter: String,
}

impl Default for State {
    fn default() -> Self {
        State { rescan: true, list: vec![], new_name: String::new(), rename: None, duplicate: None, filter: String::new() }
    }
}

fn projects_dir(app: &VgApp) -> PathBuf {
    app.prefs.projects_dir.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

fn rescan(app: &mut VgApp) {
    let recent: Vec<PathBuf> = app.prefs.recent_projects.iter().filter(|p| p.exists()).map(|p| canon(p)).collect();
    let mut all: Vec<PathBuf> = recent.clone();
    for p in projects::scan(&projects_dir(app), 2) {
        let c = canon(&p);
        if !all.contains(&c) {
            all.push(c);
        }
    }
    let open = app.project_path.as_deref().map(canon);
    let mut list: Vec<(Info, bool)> = all.into_iter().map(|p| (projects::info(&p), recent.contains(&p))).collect();
    list.sort_by(|a, b| (Some(&b.0.path) == open.as_ref()).cmp(&(Some(&a.0.path) == open.as_ref())).then(b.0.modified.cmp(&a.0.modified)));
    app.prefs_win.projects.list = list;
    app.prefs_win.projects.rescan = false;
}

/// Something to do once the list is drawn (it needs the app mutably).
type Act = Box<dyn FnOnce(&mut VgApp, &egui::Context)>;

pub fn show(app: &mut VgApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    if app.prefs_win.projects.rescan {
        rescan(app);
    }
    let dir = projects_dir(app);

    heading(ui, "New project");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.prefs_win.projects.new_name).hint_text("name, e.g. chess-engine").desired_width(220.0));
        let name = app.prefs_win.projects.new_name.trim().to_string();
        if theme::tool_button_enabled(ui, "Create and open", None, false, !name.is_empty()).clicked() {
            let poses = app.poses_path.clone().unwrap_or_else(|| PathBuf::from(crate::config::DEFAULT_POSES));
            match projects::create(&dir, &name, &poses, app.project.preset.clone(), super::super::STARTER_SCRIPT) {
                Ok(p) => {
                    app.prefs_win.projects.new_name.clear();
                    app.prefs_win.projects.rescan = true;
                    open(app, ctx, &p);
                }
                Err(e) => app.error(format!("{e:#}")),
            }
        }
        weak(ui, format!("in {}/<name>/", short(&dir)));
    });
    ui.horizontal(|ui| {
        ui.label(theme::caption("Projects folder"));
        ui.label(egui::RichText::new(short(&dir)).monospace().size(11.5));
        if theme::tool_button(ui, "Change folder", None, false).clicked() {
            super::super::dialog::open(app, super::super::dialog::Want::ProjectsDir, rfd::FileDialog::new().set_directory(&dir), super::super::dialog::How::Folder);
        }
        if app.prefs.projects_dir.is_some() && theme::tool_button(ui, "Use the working folder", None, false).clicked() {
            app.prefs.projects_dir = None;
            app.prefs_win.projects.rescan = true;
        }
        if theme::tool_button(ui, "Open from file", None, false).clicked() {
            super::super::dialog::open(app, super::super::dialog::Want::OpenProjectFrom, rfd::FileDialog::new().add_filter("videogen project", &["vgproj"]).set_directory(&dir), super::super::dialog::How::File);
        }
    });
    weak(ui, "Lists your recent projects and every project up to two folders deep in the projects folder.");

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        heading(ui, &format!("Projects · {}", app.prefs_win.projects.list.len()));
        ui.add(egui::TextEdit::singleline(&mut app.prefs_win.projects.filter).hint_text("filter").desired_width(180.0));
        if theme::tool_button(ui, "Refresh", None, false).clicked() {
            app.prefs_win.projects.rescan = true;
        }
    });
    let open_now = app.project_path.as_deref().map(canon);
    let filter = app.prefs_win.projects.filter.to_lowercase();
    let mut act: Option<Act> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("projects").num_columns(5).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
            for h in ["Project", "Folder", "Script", "Changed", ""] {
                ui.label(theme::caption(h));
            }
            ui.end_row();
            let list = app.prefs_win.projects.list.clone();
            for (info, recent) in list {
                let name = info.name();
                if !filter.is_empty() && !name.to_lowercase().contains(&filter) && !short(&info.path).to_lowercase().contains(&filter) {
                    continue;
                }
                let is_open = open_now.as_ref() == Some(&info.path);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&name).color(if is_open { theme::MARK } else { theme::WHITE }));
                    if is_open {
                        ui.label(theme::caption("open"));
                    } else if recent {
                        ui.label(egui::RichText::new("recent").size(10.0).color(theme::MUTED));
                    }
                });
                ui.label(egui::RichText::new(info.path.parent().map(short).unwrap_or_default()).monospace().size(11.0).color(theme::TEXT2));
                match (&info.error, &info.script) {
                    (Some(e), _) => ui.label(egui::RichText::new(format!("can't read: {e}")).size(10.5).color(theme::TEXT2)),
                    (None, Some(s)) => ui.label(egui::RichText::new(file_name(s)).size(11.0).color(if s.exists() { theme::TEXT2 } else { theme::MUTED })),
                    _ => ui.label(""),
                };
                theme::note(ui, ago(info.modified));
                ui.horizontal(|ui| {
                    let st = &mut app.prefs_win.projects;
                    if let Some((p, new, script)) = st.rename.as_mut().filter(|r| r.0 == info.path) {
                        ui.add(egui::TextEdit::singleline(new).desired_width(150.0));
                        ui.checkbox(script, "and its script");
                        if theme::tool_button(ui, "Rename", None, true).clicked() {
                            let (p, new, script) = (p.clone(), new.clone(), *script);
                            act = Some(Box::new(move |app, ctx| rename(app, ctx, &p, &new, script)));
                        }
                        if theme::tool_button(ui, "Cancel", None, false).clicked() {
                            st.rename = None;
                        }
                        return;
                    }
                    if let Some((p, new)) = st.duplicate.as_mut().filter(|r| r.0 == info.path) {
                        ui.add(egui::TextEdit::singleline(new).desired_width(150.0));
                        if theme::tool_button(ui, "Copy", None, true).clicked() {
                            let (p, new) = (p.clone(), new.clone());
                            act = Some(Box::new(move |app, _| {
                                match projects::duplicate(&p, &new) {
                                    Ok(n) => {
                                        remember(app, &n);
                                        app.info(format!("copied to {}", short(&n)));
                                    }
                                    Err(e) => app.error(format!("{e:#}")),
                                }
                                app.prefs_win.projects.duplicate = None;
                                app.prefs_win.projects.rescan = true;
                            }));
                        }
                        if theme::tool_button(ui, "Cancel", None, false).clicked() {
                            st.duplicate = None;
                        }
                        return;
                    }
                    if theme::tool_button_enabled(ui, "Open", None, false, !is_open && info.error.is_none()).clicked() {
                        let p = info.path.clone();
                        act = Some(Box::new(move |app, ctx| open(app, ctx, &p)));
                    }
                    if theme::tool_button_enabled(ui, "Rename", None, false, info.error.is_none()).clicked() {
                        let same = info.script.as_ref().and_then(|s| s.file_stem()).is_some_and(|s| s.to_string_lossy() == name);
                        st.rename = Some((info.path.clone(), name.clone(), same));
                        st.duplicate = None;
                    }
                    if theme::tool_button_enabled(ui, "Duplicate", None, false, info.error.is_none()).clicked() {
                        st.duplicate = Some((info.path.clone(), format!("{name}-copy")));
                        st.rename = None;
                    }
                    if recent && theme::tool_button(ui, "Forget", None, false).clicked() {
                        let p = info.path.clone();
                        act = Some(Box::new(move |app, _| {
                            app.prefs.recent_projects.retain(|r| canon(r) != p);
                            app.prefs_win.projects.rescan = true;
                        }));
                    }
                    if theme::tool_button(ui, "Delete", None, false).clicked() {
                        let p = info.path.clone();
                        act = Some(Box::new(move |app, _| delete(app, &p, is_open)));
                    }
                });
                ui.end_row();
            }
        });
    });
    if let Some(f) = act {
        f(app, ctx);
    }
}

/// Add a project to the top of the recent list.
pub fn remember(app: &mut VgApp, path: &Path) {
    let c = canon(path);
    app.prefs.recent_projects.retain(|p| canon(p) != c);
    app.prefs.recent_projects.insert(0, c);
    app.prefs.recent_projects.truncate(15);
}

/// Open a project, asking first if there are unsaved changes.
pub fn open(app: &mut VgApp, ctx: &egui::Context, path: &Path) {
    if app.dirty() {
        app.prefs_win.confirm = Some(Confirm {
            title: "Unsaved changes".into(),
            body: format!("The open project has unsaved changes. Open {} anyway?", short(path)),
            files: vec![],
            ok: "Discard them and open".into(),
            action: Action::Open(path.to_path_buf()),
        });
        return;
    }
    if let Err(e) = app.open_project(ctx, path) {
        app.error(format!("{e:#}"));
    }
}

fn rename(app: &mut VgApp, ctx: &egui::Context, path: &Path, new: &str, script: bool) {
    let is_open = app.project_path.as_deref().map(canon).as_deref() == Some(path);
    if is_open && app.dirty() {
        app.error("save the open project before renaming it");
        return;
    }
    match projects::rename(path, new, script) {
        Ok(n) => {
            for r in &mut app.prefs.recent_projects {
                if canon(r) == path {
                    *r = canon(&n);
                }
            }
            if is_open
                && let Err(e) = app.open_project(ctx, &n) {
                    app.error(format!("{e:#}"));
                }
            app.info(format!("renamed to {}", short(&n)));
        }
        Err(e) => app.error(format!("{e:#}")),
    }
    app.prefs_win.projects.rename = None;
    app.prefs_win.projects.rescan = true;
}

fn delete(app: &mut VgApp, path: &Path, is_open: bool) {
    let files = projects::files(path);
    // The words file may be shared with other scripts: not ticked by default.
    let files: Vec<(PathBuf, bool)> = files.into_iter().map(|f| {
        let words = f.to_string_lossy().ends_with(vg_project::transcripts::SUFFIX);
        (f, !words)
    }).collect();
    app.prefs_win.confirm = Some(Confirm {
        title: format!("Delete {}", path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()),
        body: format!(
            "The ticked files go to the trash (you can restore them from there). Narration recordings are kept; manage transcripts on the Transcripts page.{}",
            if is_open { " This is the open project: it's closed first, and unsaved changes are lost." } else { "" }
        ),
        files,
        ok: "Move to trash".into(),
        action: Action::Trash { closes_project: is_open },
    });
}
