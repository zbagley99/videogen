//! File dialogs, off the UI thread. Waiting for a dialog on the UI thread
//! stops the window answering the compositor, and on Wayland it can drop the
//! app. So a button starts the dialog here (`open`), the app keeps drawing,
//! and `poll` does what the button asked for once a file is chosen.

use super::{VgApp, preview::Background};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc;

pub enum How {
    File,
    Files,
    Folder,
    Save,
}

/// What to do with the chosen path(s).
pub enum Want {
    MediaImport(super::media::Kind),
    RecordingEdit,
    RecordingTrack,
    MusicFile,
    /// Transcribe: choose the audio (and open the window).
    TranscribeAudio { open: bool },
    PresetSave,
    PresetLoad,
    /// A preview background image; on cancel, go back to this background.
    PreviewBg { revert: Option<Background> },
    ProjectsDir,
    OpenProjectFrom,
    TranscriptDir,
    WordsFile,
    ExportOutput,
    SaveProjectAs { close: bool },
    SaveScriptAs,
    OpenProject,
    OpenScript,
    OpenPoseLibrary,
    PoseArt,
    PoseLibraryOut(PathBuf),
    ImportPoses,
    ImportNarration,
}

#[derive(Default)]
pub struct Dialogs {
    pending: Option<(Want, mpsc::Receiver<Vec<PathBuf>>)>,
    /// Offscreen captures: answer every dialog with these paths instead of showing one.
    pub fake: Option<Vec<PathBuf>>,
}

/// Show `dialog` and do `want` with what's chosen. One dialog at a time; while
/// one is open, asking for another does nothing.
pub fn open(app: &mut VgApp, want: Want, dialog: rfd::FileDialog, how: How) {
    if app.dialogs.pending.is_some() {
        return app.info("a file dialog is already open");
    }
    let (tx, rx) = mpsc::channel();
    if let Some(fake) = app.dialogs.fake.clone() {
        let _ = tx.send(fake);
    } else {
        let ctx = app.egui_ctx.clone();
        std::thread::spawn(move || {
            let picked = match how {
                How::File => dialog.pick_file().into_iter().collect(),
                How::Files => dialog.pick_files().unwrap_or_default(),
                How::Folder => dialog.pick_folder().into_iter().collect(),
                How::Save => dialog.save_file().into_iter().collect(),
            };
            let _ = tx.send(picked);
            ctx.request_repaint();
        });
    }
    app.dialogs.pending = Some((want, rx));
}

/// Each frame: act on a dialog that has closed.
pub fn poll(app: &mut VgApp, ctx: &egui::Context) {
    let Some((_, rx)) = &app.dialogs.pending else { return };
    let picked = match rx.try_recv() {
        Ok(p) => p,
        Err(mpsc::TryRecvError::Empty) => return,
        Err(mpsc::TryRecvError::Disconnected) => vec![],
    };
    let (want, _) = app.dialogs.pending.take().unwrap();
    // Cancelled.
    let Some(first) = picked.first().cloned() else {
        if let Want::PreviewBg { revert: Some(b) } = want {
            app.prefs.preview_bg = b;
        }
        return;
    };
    let report = |app: &mut VgApp, r: anyhow::Result<()>| {
        if let Err(e) = r {
            app.error(format!("{e:#}"));
        }
    };
    match want {
        Want::MediaImport(kind) => super::media::imported(app, kind, &picked),
        Want::RecordingEdit => super::recordings::edit(app, &first),
        Want::RecordingTrack => super::studio::add_track(app, &first, false, None),
        Want::MusicFile => super::music::add(app, &first),
        Want::TranscribeAudio { open } => {
            app.transcribe.audio = Some(first);
            app.transcribe.open |= open;
        }
        Want::PresetSave => {
            let r = app.project.preset.save(&first);
            report(app, r);
        }
        Want::PresetLoad => match vg_core::Preset::from_name_or_path(&first.to_string_lossy()) {
            Ok(p) => app.project.preset = p,
            Err(e) => app.error(format!("{e:#}")),
        },
        Want::PreviewBg { .. } => {
            app.prefs.preview_bg_image = Some(first);
            app.prefs.preview_bg = Background::Image;
        }
        Want::ProjectsDir => {
            app.prefs.projects_dir = Some(first);
            app.prefs_win.projects.rescan = true;
        }
        Want::OpenProjectFrom => super::prefs::projects::open(app, ctx, &first),
        Want::TranscriptDir => {
            app.prefs.transcript_dirs.push(first);
            app.prefs_win.transcripts.rescan = true;
        }
        Want::WordsFile => {
            let rel = super::relative_path(&first, &app.cue_dir());
            super::set_directive(&mut app.cue_text, "words", &rel.to_string_lossy());
            app.reparse();
        }
        Want::ExportOutput => app.project.export.output = super::relative_path(&first, &app.base()),
        Want::SaveProjectAs { close } => {
            let r = app.save_project_to(&super::with_ext(first, "vgproj"));
            report(app, r);
            if close && !app.dirty() {
                app.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        Want::SaveScriptAs => {
            app.cue_path = Some(first);
            let r = app.save_script().map(|_| ());
            report(app, r);
        }
        Want::OpenProject => {
            let r = app.open_project(ctx, &first);
            report(app, r);
        }
        Want::OpenScript => {
            let r = app.open_script(&first);
            report(app, r);
        }
        Want::OpenPoseLibrary => {
            let r = app.load_poses(ctx, &first);
            report(app, r);
        }
        Want::PoseArt => {
            let d = rfd::FileDialog::new().set_title("Save pose library").add_filter("pose library", &["ron"]).set_directory(app.base().join("avatar")).set_file_name("poses.ron");
            open(app, Want::PoseLibraryOut(first), d, How::Save);
        }
        Want::PoseLibraryOut(art) => {
            let r = app.create_pose_library_at(ctx, &art, super::with_ext(first, "ron"));
            report(app, r);
        }
        Want::ImportPoses => {
            let r = app.import_poses_from(ctx, &first);
            report(app, r);
        }
        Want::ImportNarration => app.import_audio_from(&first),
    }
}
