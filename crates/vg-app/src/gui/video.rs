//! The project's video track in the app: adding media to the timeline, and
//! edits with undo/redo. The track itself (`vg_project::video`) lives in the
//! project file; the timeline draws and edits it, the preview shows it.

use super::util::file_name;
use super::{VgApp, media::Kind};
use eframe::egui;
use std::path::{Path, PathBuf};
use vg_project::video::{VideoClip, VideoTrack};

/// What dragged pose flags (and other timeline drags) snap to.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SnapTo {
    /// Word transitions, line cuts, pose changes and the playhead.
    #[default]
    Captions,
    /// Video clip edges and cuts, and the playhead.
    Video,
}

/// Which kind of edit an undo step belongs to, so one Ctrl+Z undoes the most
/// recent edit whichever lane it was made in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    /// A narration cut (`studio::timeline_cut`; its snapshot is in `studio.cut_undo`).
    NarrationCut,
    /// A video track edit (its snapshot is in `Video::undo`).
    Video,
}

#[derive(Default)]
pub struct Video {
    /// Track states before each video edit (newest last).
    undo: Vec<VideoTrack>,
    redo: Vec<VideoTrack>,
    /// The order edits were made in, across lanes (newest last).
    pub history: Vec<EditKind>,
    /// Selected clip on the video lane.
    pub selected: Option<usize>,
}

const MAX_UNDO: usize = 100;

/// Change the video track as one undoable step. `f` returns false when it
/// changed nothing (no step is recorded then).
pub fn edit(app: &mut VgApp, f: impl FnOnce(&mut VideoTrack) -> bool) -> bool {
    let before = app.project.video.clone();
    if !f(&mut app.project.video) {
        app.project.video = before;
        return false;
    }
    commit(app, before)
}

/// Record the track as it was `before` changes already made to it (a live
/// timeline drag) as one undoable step. False when nothing changed.
pub fn commit(app: &mut VgApp, before: VideoTrack) -> bool {
    if app.project.video == before {
        return false;
    }
    app.video.undo.push(before);
    if app.video.undo.len() > MAX_UNDO {
        app.video.undo.remove(0);
    }
    app.video.redo.clear();
    note(app, EditKind::Video);
    if app.video.selected.is_some_and(|i| i >= app.project.video.clips.len()) {
        app.video.selected = None;
    }
    true
}

/// Record that an edit of `kind` happened (for the shared Ctrl+Z order).
/// A narration cut is a new edit too: the video redo steps go.
pub fn note(app: &mut VgApp, kind: EditKind) {
    app.video.history.push(kind);
    if app.video.history.len() > 2 * MAX_UNDO {
        app.video.history.remove(0);
    }
    if kind == EditKind::NarrationCut {
        app.video.redo.clear();
    }
}

/// Which lane the next Ctrl+Z undoes: the newest edit in `history` that can
/// still be undone (`cut`, `video`: whether each lane has an undo step left).
/// Entries whose steps are gone (trimmed) are passed over.
pub fn next_undo(history: &[EditKind], cut: bool, video: bool) -> Option<EditKind> {
    let can = |k: &EditKind| match k {
        EditKind::NarrationCut => cut,
        EditKind::Video => video,
    };
    history.iter().rev().find(|k| can(k)).copied().or(if video {
        Some(EditKind::Video)
    } else if cut {
        Some(EditKind::NarrationCut)
    } else {
        None
    })
}

/// Forget the newest `kind` entry (its step was undone).
pub fn forget(app: &mut VgApp, kind: EditKind) {
    if let Some(k) = app.video.history.iter().rposition(|k| *k == kind) {
        app.video.history.remove(k);
    }
}

pub fn can_undo(app: &VgApp) -> bool {
    !app.video.undo.is_empty()
}

pub fn can_redo(app: &VgApp) -> bool {
    !app.video.redo.is_empty()
}

pub fn undo(app: &mut VgApp) {
    let Some(prev) = app.video.undo.pop() else { return };
    let now = std::mem::replace(&mut app.project.video, prev);
    app.video.redo.push(now);
    forget(app, EditKind::Video);
    app.video.selected = app.video.selected.filter(|&i| i < app.project.video.clips.len());
}

pub fn redo(app: &mut VgApp) -> bool {
    let Some(next) = app.video.redo.pop() else { return false };
    let now = std::mem::replace(&mut app.project.video, next);
    app.video.undo.push(now);
    note(app, EditKind::Video);
    app.video.selected = app.video.selected.filter(|&i| i < app.project.video.clips.len());
    true
}

/// A clip's source file, resolved against the project.
pub fn source_path(app: &VgApp, clip: &VideoClip) -> PathBuf {
    vg_project::resolve(&app.base(), &clip.source)
}

/// Dragged from a Media card; the timeline adds it where it's dropped.
#[derive(Clone, Debug)]
pub struct MediaDrop(pub PathBuf);

/// Put a media file on the timeline at timeline time `at` (the playhead, or
/// where it was dropped), in its kind's section: video and images on the
/// video track, audio as a track in the narration's edit (shown in the Audio
/// section, mixed under the voice).
pub fn add_media(app: &mut VgApp, ctx: &egui::Context, path: &Path, at: f64) {
    let Some(kind) = super::media::kind_of(path) else {
        return app.error(format!("{} isn't an image, video or audio file", path.display()));
    };
    let name = file_name(path);
    match kind {
        Kind::Video | Kind::Image => {
            let still = kind == Kind::Image;
            let len = if still {
                None
            } else {
                match vg_media::probe_info(path) {
                    Ok(i) if i.video.is_some() => i.duration,
                    Ok(_) => return app.error(format!("{name} has no picture")),
                    Err(e) => return app.error(format!("{e:#}")),
                }
            };
            let rel = super::relative_path(path, &app.base());
            let mut placed = None;
            edit(app, |track| {
                placed = Some(track.insert(VideoClip::new(rel, at, len, still), at));
                true
            });
            if let Some(i) = placed {
                let c = &app.project.video.clips[i];
                let (start, len) = (c.start, c.len());
                app.video.selected = Some(i);
                app.info(format!("added {name} to the Video section at {} ({}) · Ctrl+Z undoes it", vg_core::format_time(start), vg_core::format_time(len)));
            }
        }
        Kind::Audio => {
            super::studio::ensure_loaded(app);
            if app.studio.mix.tracks.is_empty() {
                return app.error("add a narration first (File › Import narration, or Record): other audio is mixed under it");
            }
            let before = app.studio.mix.tracks.len();
            super::studio::add_track(app, path, false, Some(at));
            if app.studio.mix.tracks.len() > before {
                // Heard in the Video tab once the narration plays the edit's mix.
                if super::studio::narration_state(app) == super::studio::Narration::Unused {
                    super::studio::sync_narration(app, ctx);
                }
                app.info(format!("added {name} to the Audio section at {} · adjust it in the Audio tab", vg_core::format_time(at)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use EditKind::*;

    #[test]
    fn undo_follows_the_order_edits_were_made_in() {
        assert_eq!(next_undo(&[Video, NarrationCut], true, true), Some(NarrationCut));
        assert_eq!(next_undo(&[NarrationCut, Video], true, true), Some(Video));
        // The newest cut's snapshot is gone: the video edit before it is next.
        assert_eq!(next_undo(&[Video, NarrationCut], false, true), Some(Video));
        // Steps with no history entry still undo.
        assert_eq!(next_undo(&[], true, false), Some(NarrationCut));
        assert_eq!(next_undo(&[NarrationCut], false, false), None);
    }
}
