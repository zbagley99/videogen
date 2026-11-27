//! The Settings window: preferences, projects, transcripts, and storage &
//! caches, one page each.

mod general;
pub(super) mod projects;
mod storage;
pub(super) mod transcripts;

use super::util::{plural, short};
use super::{VgApp, place, theme};
use eframe::egui;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Page {
    #[default]
    General,
    Projects,
    Transcripts,
    Storage,
}

const PAGES: &[(Page, &str, &str)] = &[
    (Page::General, "General", "Recording, transcription, playback, editing"),
    (Page::Projects, "Projects", "Open, create, rename, copy, delete"),
    (Page::Transcripts, "Transcripts", "Edit, bulk-edit, clean up"),
    (Page::Storage, "Storage & cache", "Caches, models, disk use"),
];

#[derive(Default)]
pub struct PrefsWindow {
    pub open: bool,
    page: Page,
    general: general::State,
    pub(super) projects: projects::State,
    pub(super) transcripts: transcripts::State,
    storage: storage::State,
    confirm: Option<Confirm>,
}

impl PrefsWindow {
    pub fn show_page(&mut self, page: Page) {
        self.open = true;
        self.page = page;
    }
}

/// A question before doing something that's hard to undo.
struct Confirm {
    title: String,
    body: String,
    /// Files involved, each with a tick box (for moving to the trash).
    files: Vec<(PathBuf, bool)>,
    ok: String,
    action: Action,
}

enum Action {
    /// Move the ticked files to the trash; then, if it was the open project, start a new one.
    Trash { closes_project: bool },
    /// Delete a folder for good (a cache that can be downloaded again).
    DeleteDir(PathBuf),
    ResetPrefs,
    /// Open a project, dropping unsaved changes (or saving them first).
    Open(PathBuf),
}

pub fn window(app: &mut VgApp, ctx: &egui::Context) {
    transcripts::background(app);
    confirm(app, ctx);
    if !app.prefs_win.open {
        return;
    }
    let mut open = true;
    let pos = app.place.place(ctx, "prefs", egui::Align2::CENTER_CENTER, egui::vec2(1000.0, 660.0));
    place::at(egui::Window::new(theme::caption("Settings").color(theme::WHITE)), "prefs", pos)
        .open(&mut open)
        .resizable(true)
        .collapsible(false)
        .default_size([980.0, 620.0])
        .min_size([760.0, 420.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    for (p, name, blurb) in PAGES {
                        let on = app.prefs_win.page == *p;
                        let r = ui.add(egui::Button::new(egui::RichText::new(*name).color(if on { theme::BLACK } else { theme::WHITE })).fill(if on { theme::WHITE } else { theme::BLACK }).min_size(egui::vec2(170.0, 26.0)));
                        if r.clicked() {
                            app.prefs_win.page = *p;
                        }
                        ui.label(egui::RichText::new(*blurb).size(10.0).color(theme::MUTED));
                        ui.add_space(6.0);
                    }
                });
                ui.separator();
                ui.vertical(|ui| match app.prefs_win.page {
                    Page::General => general::show(app, ui),
                    Page::Projects => projects::show(app, ui, ctx),
                    Page::Transcripts => transcripts::show(app, ui, ctx),
                    Page::Storage => storage::show(app, ui),
                });
            });
        });
    app.prefs_win.open = open;
}

fn confirm(app: &mut VgApp, ctx: &egui::Context) {
    let Some(c) = &mut app.prefs_win.confirm else { return };
    let mut choice: Option<bool> = None;
    let mut save_first = false;
    egui::Modal::new(egui::Id::new("prefs-confirm")).show(ctx, |ui| {
        ui.set_max_width(560.0);
        ui.label(theme::caption(&c.title).color(theme::WHITE));
        ui.add_space(4.0);
        ui.label(egui::RichText::new(&c.body).color(theme::TEXT2));
        if !c.files.is_empty() {
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for (f, on) in &mut c.files {
                    ui.checkbox(on, egui::RichText::new(short(f)).monospace().size(11.0));
                }
            });
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let any = c.files.is_empty() || c.files.iter().any(|f| f.1);
            if matches!(c.action, Action::Open(_))
                && theme::tool_button(ui, "Save, then open", None, false).clicked() {
                    save_first = true;
                    choice = Some(true);
                }
            if theme::tool_button_enabled(ui, &c.ok, None, false, any).clicked() {
                choice = Some(true);
            }
            if theme::tool_button(ui, "Cancel", None, false).clicked() {
                choice = Some(false);
            }
        });
    });
    let Some(yes) = choice else { return };
    let c = app.prefs_win.confirm.take().unwrap();
    if !yes {
        return;
    }
    match c.action {
        Action::Trash { closes_project } => {
            let files: Vec<PathBuf> = c.files.into_iter().filter(|f| f.1).map(|f| f.0).collect();
            match trash::delete_all(&files) {
                Ok(()) => {
                    if closes_project {
                        app.new_project(ctx);
                    }
                    app.prefs.recent_projects.retain(|p| p.exists());
                    app.info(format!("moved {} file{} to the trash", files.len(), plural(files.len())));
                }
                Err(e) => app.error(format!("moving to the trash: {e}")),
            }
            app.prefs_win.projects.rescan = true;
            app.prefs_win.transcripts.rescan = true;
        }
        Action::DeleteDir(dir) => {
            match storage::delete_cache_dir(&dir) {
                Ok(()) => app.info(format!("deleted {}", dir.display())),
                Err(e) => app.error(format!("deleting {}: {e}", dir.display())),
            }
            app.prefs_win.storage.rescan = true;
        }
        Action::ResetPrefs => general::reset(app),
        Action::Open(path) => {
            if save_first {
                match app.save_project(false) {
                    Ok(true) => {}
                    // Asking where to save it first; open the other one after.
                    Ok(false) => return,
                    Err(e) => {
                        app.error(format!("{e:#}"));
                        return;
                    }
                }
            }
            if let Err(e) = app.open_project(ctx, &path) {
                app.error(format!("{e:#}"));
            }
        }
    }
}

// ---- small helpers shared by the pages ----------------------------------------------

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(theme::caption(text).color(theme::WHITE));
    ui.add_space(2.0);
}
