//! Transport controls and the timeline, in labelled sections: Audio (the
//! narration's waveform and the Audio tab's tracks), Transcript (script lines),
//! Captions (caption word timings), Video (the project's video track) and
//! Poses (flags). Click/drag to scrub; drag a flag to retime its tag (the
//! script gets `@time`); right-click a flag to delete it; drag a word's left
//! edge to change when its caption highlight starts, or its body to move it;
//! scroll to zoom. Snapping (Off / Captions / Video) pulls drags toward word
//! transitions, line cuts and pose changes, or toward video clip edges, and
//! the playhead (hold Alt to place freely).
//!
//! Cutting, in the Audio section: drag to select a stretch, click a clip to
//! select it, drag a clip's edge to trim it; Delete removes the selection, S
//! splits at the playhead. Everything after a cut moves up, script and words
//! included.
//!
//! Editing video, in the Video section: click a clip to select it, drag it to
//! move it, drag an edge to trim it, drag the triangles at its top corners to
//! set fades; drag across a gap (or Shift+drag anywhere) to select a stretch.
//! S splits, Delete lifts (leaving a gap), Shift+Delete ripples, M mutes.
//! Keys act on the section last clicked; Ctrl+Z undoes the newest edit in
//! either section.

use super::util::{file_name, plural};
use super::{VgApp, media, script, studio, theme, video};
use crate::recorder::PEAK_RATE;
use eframe::egui;
use vg_core::captions::LineWords;
use vg_core::{Action, Word, format_time};
use vg_project::video::{STILL_SECONDS, VideoTrack};

/// The section keys (S, Delete, M, Esc) act on: the one last clicked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Focus {
    #[default]
    Audio,
    Video,
}


/// A dragged file and its length, once probed.
type Probed = (std::path::PathBuf, Option<f64>);

#[derive(Default)]
pub struct TimelineView {
    /// Visible window: start time and seconds shown.
    start: f64,
    span: Option<f64>,
    /// Marker being dragged: (event index, current time).
    dragging: Option<(usize, f64)>,
    /// Caption word being dragged.
    word_drag: Option<WordDrag>,
    /// Where the current drag snapped to, for the guide line.
    snapped: Option<f64>,
    /// The end-of-video handle being dragged: its current time.
    end_drag: Option<f64>,
    /// Selected stretch of the narration (video seconds).
    sel: Option<(f64, f64)>,
    sel_drag: Option<SelDrag>,
    /// A clip edge being dragged to trim it.
    trim: Option<Trim>,
    /// Which section the keys act on.
    focus: Focus,
    /// Selected stretch of the video track.
    vsel: Option<(f64, f64)>,
    /// Dragging out a video selection from here.
    vsel_drag: Option<f64>,
    /// A video clip being moved, trimmed or faded.
    vdrag: Option<VideoDrag>,
    /// The clip the context menu is for.
    menu_clip: Option<usize>,
    /// The track before the transport's fade fields started changing it (one undo step).
    fade_before: Option<VideoTrack>,
    /// Lengths of video files being dragged from the Media tab (None while
    /// probing, or if it failed), so the drop ghost is the clip's real size.
    drop_lens: std::collections::HashMap<std::path::PathBuf, Option<f64>>,
    drop_probe: Option<(std::sync::mpsc::Sender<Probed>, std::sync::mpsc::Receiver<Probed>)>,
}

#[derive(Clone, Copy)]
enum SelDrag {
    /// Dragging out a new selection from here.
    Range(f64),
    /// Moving one edge of the selection (true: the end).
    Edge(bool),
}

#[derive(Clone, Copy)]
struct Trim {
    /// Where the edge was.
    at: f64,
    /// How far it can go.
    lo: f64,
    hi: f64,
    /// Where it is now.
    t: f64,
}

struct WordDrag {
    /// Index into the timeline's spans.
    span: usize,
    /// Word within the line.
    idx: usize,
    /// Moving the whole word (vs. its start edge).
    body: bool,
    /// Pointer time minus word start at grab (body drags).
    grab: f64,
    /// The line's words as edited so far.
    words: Vec<Word>,
}

/// What part of a video clip the pointer has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grab {
    Move,
    Start,
    End,
    FadeIn,
    FadeOut,
}

struct VideoDrag {
    i: usize,
    grab: Grab,
    /// Pointer time minus the clip's start at grab.
    offset: f64,
    /// The track before the drag; each frame re-applies the drag to it, and
    /// the release records it as one undo step.
    before: VideoTrack,
}

/// Snap `t` to the nearest target within `radius` seconds.
fn snap(t: f64, targets: &[f64], radius: f64) -> (f64, Option<f64>) {
    match targets.iter().copied().min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())) {
        Some(s) if (s - t).abs() <= radius => (s, Some(s)),
        _ => (t, None),
    }
}

/// Snap a block `start..start + len` by whichever edge is nearer a target:
/// (new start, the target it snapped to).
fn snap_span(start: f64, len: f64, targets: &[f64], radius: f64) -> (f64, Option<f64>) {
    let (s, hs) = snap(start, targets, radius);
    let (e, he) = snap(start + len, targets, radius);
    match (hs, he) {
        (Some(_), Some(_)) if (s - start).abs() <= (e - len - start).abs() => (s, hs),
        (_, Some(_)) => (e - len, he),
        (Some(_), None) => (s, hs),
        _ => (start, None),
    }
}

/// What a press at `x`, `y` grabs of a clip drawn from `x0` to `x1` (top at
/// `top`, fade handles at `fi` and `fo`): the fade handles in the top band,
/// then the edges, then the body.
fn grab_at(x: f32, y: f32, x0: f32, x1: f32, top: f32, fi: f32, fo: f32) -> Grab {
    let band = y <= top + 10.0;
    if band && (x - fi).abs() <= 6.0 && (x - fi).abs() <= (x - fo).abs() {
        return Grab::FadeIn;
    }
    if band && (x - fo).abs() <= 6.0 {
        return Grab::FadeOut;
    }
    // Narrow clips keep a middle to move them by.
    let edge = 5.0_f32.min((x1 - x0) / 3.0);
    if x - x0 <= edge {
        Grab::Start
    } else if x1 - x <= edge {
        Grab::End
    } else {
        Grab::Move
    }
}

impl TimelineView {
    pub fn fit_all(&mut self) {
        self.start = 0.0;
        self.span = None;
    }
}

/// Make a cut and report it; the selection goes (its times have moved).
fn cut(app: &mut VgApp, ctx: &egui::Context, cut: studio::Cut) {
    use studio::Cut;
    app.stop();
    match studio::timeline_cut(app, ctx, cut) {
        Ok(msg) => {
            app.timeline.sel = None;
            match cut {
                Cut::Delete(a, b) => app.seek(a.min(b)),
                Cut::Keep(..) => app.seek(0.0),
                Cut::Split(_) => {}
            }
            app.info(format!("{msg} · Ctrl+Z undoes it"));
        }
        Err(e) => app.error(e),
    }
}

fn undo_cut(app: &mut VgApp) {
    match studio::undo_cut(app) {
        Ok(()) => {
            app.timeline.sel = None;
            app.info("cut undone");
        }
        Err(e) => app.error(e),
    }
}

/// Whether Ctrl+Z has anything to undo, in either section.
fn can_undo(app: &VgApp) -> bool {
    studio::has_cut_undo(app) || video::can_undo(app)
}

/// Undo the newest edit, a narration cut or a video edit.
fn undo(app: &mut VgApp) {
    use video::EditKind;
    match video::next_undo(&app.video.history, studio::has_cut_undo(app), video::can_undo(app)) {
        Some(EditKind::NarrationCut) => undo_cut(app),
        Some(EditKind::Video) => {
            video::undo(app);
            app.timeline.vsel = None;
            app.video.selected = None;
            app.info("video edit undone · Ctrl+Shift+Z redoes it");
        }
        None => {}
    }
}

/// Redo the last undone video edit (narration cuts have no redo).
fn redo(app: &mut VgApp) {
    if video::redo(app) {
        app.timeline.vsel = None;
        app.video.selected = None;
        app.info("video edit redone");
    } else {
        app.info("nothing to redo (narration cuts can't be redone: cut again)");
    }
}

/// An edit of the video track, from the keys, the transport or a clip's menu.
#[derive(Clone, Copy)]
enum VideoAct {
    /// Split the clip under the playhead.
    Split,
    /// Remove the selected stretch or clip; true: ripple (close the gap).
    Delete(bool),
    /// Mute or unmute the selected clip (or the one under the playhead).
    Mute,
    RemoveFades,
    /// Show the Media tab (where the clip's file is).
    Reveal,
}

fn video_act(app: &mut VgApp, act: VideoAct) {
    let fin = " · Ctrl+Z undoes it";
    match act {
        VideoAct::Split => {
            let t = app.time;
            let Some(i) = app.project.video.at(t) else {
                return app.info("no video clip under the playhead to split");
            };
            if video::edit(app, |tr| tr.split(t)) {
                app.video.selected = Some(i + 1);
                app.info(format!("split the clip at {}{fin}", format_time(t)));
            } else {
                app.info("too close to the clip's edge to split");
            }
        }
        VideoAct::Delete(ripple) => {
            let how = if ripple { "and closed the gap" } else { "(a gap is left · Shift+Delete closes it)" };
            if let Some((a, b)) = app.timeline.vsel.take() {
                if video::edit(app, |tr| {
                    tr.delete_range(a, b, ripple);
                    true
                }) {
                    app.video.selected = None;
                    app.info(format!("removed {} of video {how}{fin}", format_time(b - a)));
                } else {
                    app.info("no video in the selection");
                }
            } else if let Some(i) = app.video.selected.take()
                && video::edit(app, |tr| {
                    tr.remove(i, ripple);
                    true
                }) {
                    app.info(format!("removed the clip {how}{fin}"));
                }
        }
        VideoAct::Mute => {
            let Some(i) = app.video.selected.or_else(|| app.project.video.at(app.time)) else {
                return app.info("select a clip to mute");
            };
            if app.project.video.clips[i].still {
                return app.info("a still image has no sound");
            }
            video::edit(app, |tr| {
                tr.clips[i].muted = !tr.clips[i].muted;
                true
            });
            let muted = app.project.video.clips[i].muted;
            app.info(format!("{} the clip's sound{fin}", if muted { "muted" } else { "unmuted" }));
        }
        VideoAct::RemoveFades => {
            if let Some(i) = app.video.selected
                && video::edit(app, |tr| {
                    tr.set_fades(i, 0.0, 0.0);
                    true
                }) {
                    app.info(format!("removed the fades{fin}"));
                }
        }
        VideoAct::Reveal => {
            app.prefs.sidebar = media::Sidebar::Media;
            if let Some(c) = app.video.selected.and_then(|i| app.project.video.clips.get(i)) {
                let name = clip_name(c);
                app.info(format!("{name} is in the Media tab"));
            }
        }
    }
}

fn clip_name(c: &vg_project::video::VideoClip) -> String {
    c.source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| c.source.display().to_string())
}

/// The timeline's keys (from `handle_shortcuts`, when no text field has the
/// keyboard): S splits, Delete removes (Shift+Delete ripples, for video), M
/// mutes a clip, Esc drops selections, Ctrl+Z undoes the newest edit in
/// either section, Ctrl+Shift+Z / Ctrl+Y redo a video edit. S, Delete and M
/// act on the focused section.
pub fn keys(app: &mut VgApp, ctx: &egui::Context) {
    use egui::{Key, Modifiers};
    let none = Modifiers::NONE;
    let on_video = app.timeline.focus == Focus::Video;
    let has_vtarget = app.timeline.vsel.is_some() || app.video.selected.is_some();
    let can_del = if on_video { has_vtarget } else { app.timeline.sel.is_some() };
    let can_esc = app.timeline.sel.is_some() || has_vtarget;
    let (can_redo, can_undo) = (video::can_redo(app), can_undo(app));
    let (redo, undo, split, ripple, del, mute, esc) = ctx.input_mut(|i| {
        // Most specific first: consume_key ignores extra Shift.
        let redo = can_redo && (i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z) || i.consume_key(Modifiers::COMMAND, Key::Y));
        let shift = i.modifiers.shift;
        (
            redo,
            can_undo && !shift && i.consume_key(Modifiers::COMMAND, Key::Z),
            i.consume_key(none, Key::S),
            on_video && can_del && (i.consume_key(Modifiers::SHIFT, Key::Delete) || i.consume_key(Modifiers::SHIFT, Key::Backspace)),
            can_del && (i.consume_key(none, Key::Delete) || i.consume_key(none, Key::Backspace)),
            on_video && i.consume_key(none, Key::M),
            can_esc && i.consume_key(none, Key::Escape),
        )
    });
    if split {
        if on_video {
            video_act(app, VideoAct::Split);
        } else if studio::narration_segments(app).is_some() {
            cut(app, ctx, studio::Cut::Split(app.time));
        }
    }
    if ripple {
        video_act(app, VideoAct::Delete(true));
    } else if del {
        if on_video {
            video_act(app, VideoAct::Delete(false));
        } else if let Some((a, b)) = app.timeline.sel {
            cut(app, ctx, studio::Cut::Delete(a, b));
        }
    }
    if mute {
        video_act(app, VideoAct::Mute);
    }
    if esc {
        app.timeline.sel = None;
        app.timeline.vsel = None;
        app.video.selected = None;
    }
    if undo {
        self::undo(app);
    }
    if redo {
        self::redo(app);
    }
}

/// The timeline's state for `vg screenshot` checks: focus, snapping,
/// selections, the shared undo order and the video track.
pub fn debug(app: &VgApp) -> String {
    let snap = if app.prefs.snap_timeline { format!("{:?}", app.prefs.snap_to) } else { "Off".into() };
    let mut out = format!(
        "timeline focus={:?} snap={snap} sel={:?} vsel={:?} selected={:?} history={:?} redo={}\n",
        app.timeline.focus,
        app.timeline.sel.map(|(a, b)| (round2(a), round2(b))),
        app.timeline.vsel.map(|(a, b)| (round2(a), round2(b))),
        app.video.selected,
        app.video.history,
        video::can_redo(app),
    );
    for (i, c) in app.project.video.clips.iter().enumerate() {
        out += &format!(
            "video[{i}] {} start={:.2} end={:.2} src={:.2}..{:.2}{}{} fade={:.2}/{:.2}\n",
            clip_name(c),
            c.start,
            c.end(),
            c.src_in,
            c.src_out,
            if c.still { " still" } else { "" },
            if c.muted { " muted" } else { "" },
            c.fade_in,
            c.fade_out
        );
    }
    out
}

fn round2(t: f64) -> f64 {
    (t * 100.0).round() / 100.0
}

const RULER_H: f32 = 20.0;
const WAVE_H: f32 = 44.0;
const LINES_H: f32 = 26.0;
const WORDS_H: f32 = 28.0;
const VIDEO_H: f32 = 40.0;
const MARK_H: f32 = 50.0;
/// One row of the clips lane (the Audio tab's tracks).
const CLIP_ROW: f32 = 12.0;
/// The section header gutter on the left (icons only when the panel is narrow).
const GUTTER: f32 = 100.0;
const GUTTER_NARROW: f32 = 26.0;

/// The timeline's sections, top to bottom.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Audio,
    Transcript,
    Captions,
    Video,
    Poses,
}

impl Lane {
    fn name(self) -> &'static str {
        match self {
            Lane::Audio => "AUDIO",
            Lane::Transcript => "TRANSCRIPT",
            Lane::Captions => "CAPTIONS",
            Lane::Video => "VIDEO",
            Lane::Poses => "POSES",
        }
    }
}

/// A section's little key glyph, centred on `c`.
fn lane_icon(p: &egui::Painter, c: egui::Pos2, lane: Lane, color: egui::Color32) {
    let s = egui::Stroke::new(1.0_f32, color);
    let pt = |x: f32, y: f32| c + egui::vec2(x, y);
    match lane {
        // A little waveform.
        Lane::Audio => {
            for (x, h) in [(-4.0, 1.5), (-2.0, 3.5), (0.0, 5.0), (2.0, 2.5), (4.0, 1.0)] {
                p.line_segment([pt(x, -h), pt(x, h)], s);
            }
        }
        // Lines of text.
        Lane::Transcript => {
            for (y, w) in [(-3.0, 10.0), (0.0, 7.0), (3.0, 9.0)] {
                p.line_segment([pt(-5.0, y), pt(-5.0 + w, y)], s);
            }
        }
        // A caption with one word lit.
        Lane::Captions => {
            p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(11.0, 7.0)), 0.0, s, egui::StrokeKind::Inside);
            p.rect_filled(egui::Rect::from_min_size(pt(-3.5, -1.5), egui::vec2(4.0, 3.0)), 0.0, color);
        }
        // A frame of film with its sprocket holes.
        Lane::Video => {
            p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(11.0, 9.0)), 0.0, s, egui::StrokeKind::Inside);
            for x in [-3.0, 0.0, 3.0] {
                p.rect_filled(egui::Rect::from_center_size(pt(x, -2.8), egui::vec2(1.2, 1.2)), 0.0, color);
                p.rect_filled(egui::Rect::from_center_size(pt(x, 2.8), egui::vec2(1.2, 1.2)), 0.0, color);
            }
        }
        // A flag.
        Lane::Poses => {
            p.line_segment([pt(-4.0, -5.0), pt(-4.0, 5.0)], s);
            p.rect_filled(egui::Rect::from_min_size(pt(-4.0, -5.0), egui::vec2(8.0, 5.0)), 0.0, color);
        }
    }
}

/// Uppercase, tracked label text for the gutter.
fn tracked(p: &egui::Painter, text: &str, size: f32, color: egui::Color32) -> std::sync::Arc<egui::Galley> {
    let fmt = egui::TextFormat { font_id: egui::FontId::proportional(size), color, extra_letter_spacing: 1.0, ..Default::default() };
    p.layout_job(egui::text::LayoutJob::single_section(text.to_string(), fmt))
}

/// A crossed-out speaker, centred on `c` (a muted clip).
fn muted_glyph(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let pt = |x: f32, y: f32| c + egui::vec2(x, y);
    p.add(egui::Shape::convex_polygon(vec![pt(-5.0, -1.5), pt(-3.0, -1.5), pt(0.0, -4.0), pt(0.0, 4.0), pt(-3.0, 1.5), pt(-5.0, 1.5)], color, egui::Stroke::NONE));
    let s = egui::Stroke::new(1.2_f32, color);
    p.line_segment([pt(2.0, -2.0), pt(5.5, 2.0)], s);
    p.line_segment([pt(2.0, 2.0), pt(5.5, -2.0)], s);
}

/// A small tag with black text on white (lengths, times).
fn tag(p: &egui::Painter, center: egui::Pos2, text: &str, fill: egui::Color32) -> egui::Rect {
    let g = p.layout_no_wrap(text.to_string(), egui::FontId::monospace(9.5), theme::BLACK);
    let r = egui::Rect::from_center_size(center, g.size() + egui::vec2(10.0, 4.0));
    p.rect_filled(r, 0.0, fill);
    p.galley(r.center() - g.size() / 2.0, g, theme::BLACK);
    r
}

pub fn show(app: &mut VgApp, ui: &mut egui::Ui) {
    transport(app, ui);
    ui.add_space(2.0);
    let ctx = ui.ctx().clone();

    let duration = app.duration().max(0.1);
    let audio_len = app.audio.as_ref().map(|a| a.duration);
    // The view reaches a little past the end (or the narration or the video
    // track, if longer), so the end handle can be dragged out and every clip
    // can be reached.
    // (While a clip is dragged the view keeps the track's old end, or it
    // would rescale under the pointer.)
    let video_end = app.timeline.vdrag.as_ref().map(|d| d.before.end()).unwrap_or(app.project.video.end());
    let total = duration.max(audio_len.unwrap_or(0.0)).max(video_end) * 1.06 + 1.0;
    let span = app.timeline.span.unwrap_or(total).clamp(0.5, total);
    app.timeline.start = app.timeline.start.clamp(0.0, (total - span).max(0.0));

    let strips = studio::clip_strips(app);
    let clip_rows = strips.len().min(4) as f32;
    let width = ui.available_width();
    // Fill the panel exactly (asking for less would shrink the panel frame by
    // frame), and scale every lane with it, not just the waveform.
    let height = ui.available_height().max(RULER_H + 40.0);
    let base = WAVE_H + LINES_H + WORDS_H + VIDEO_H + MARK_H + if clip_rows > 0.0 { clip_rows * CLIP_ROW + 4.0 } else { 0.0 };
    let k = ((height - RULER_H) / base).max(0.5);
    // Text lanes grow a little slower than the panel; the waveform takes the rest.
    let kl = if k > 1.0 { k.powf(0.75) } else { k };
    // Type grows more gently than the lanes.
    let fs = kl.sqrt().clamp(0.85, 1.5);
    let clip_row = CLIP_ROW * kl;
    let clips_h = if clip_rows > 0.0 { clip_rows * clip_row + 4.0 * kl } else { 0.0 };
    let wave_h = (height - RULER_H - clips_h - (LINES_H + WORDS_H + VIDEO_H + MARK_H) * kl).max(WAVE_H * 0.5);
    let (full, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    // Section names on the left; everything else maps time across the rest.
    let gw = if full.width() < 700.0 { GUTTER_NARROW } else { GUTTER };
    let gutter = egui::Rect::from_min_max(full.min, egui::pos2(full.left() + gw, full.bottom()));
    let rect = egui::Rect::from_min_max(egui::pos2(gutter.right(), full.top()), full.max);
    let response = ui.interact(rect, ui.id().with("timeline-lanes"), egui::Sense::click_and_drag());
    let gutter_resp = ui.interact(gutter, ui.id().with("timeline-gutter"), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let gp = ui.painter_at(gutter);
    painter.rect_filled(rect, 0.0, theme::BLACK);

    let start = app.timeline.start;
    let x_of = |t: f64| rect.left() + ((t - start) / span) as f32 * rect.width();
    let t_of = |x: f32| start + ((x - rect.left()) / rect.width()) as f64 * span;

    // Lane rects.
    let ruler = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), RULER_H));
    let wave = egui::Rect::from_min_size(ruler.left_bottom(), egui::vec2(rect.width(), wave_h));
    let clips_lane = egui::Rect::from_min_size(wave.left_bottom(), egui::vec2(rect.width(), clips_h));
    let lines = egui::Rect::from_min_size(clips_lane.left_bottom(), egui::vec2(rect.width(), LINES_H * kl));
    let words_lane = egui::Rect::from_min_size(lines.left_bottom(), egui::vec2(rect.width(), WORDS_H * kl));
    let vlane = egui::Rect::from_min_size(words_lane.left_bottom(), egui::vec2(rect.width(), VIDEO_H * kl));
    let marks = egui::Rect::from_min_size(vlane.left_bottom(), egui::vec2(rect.width(), MARK_H * kl));
    let audio_sec = wave.union(clips_lane);
    let sections = [(Lane::Audio, audio_sec), (Lane::Transcript, lines), (Lane::Captions, words_lane), (Lane::Video, vlane), (Lane::Poses, marks)];
    // Alternate the sections' backgrounds so each reads as a band.
    for (n, (_, r)) in sections.iter().enumerate() {
        if n % 2 == 1 {
            painter.rect_filled(*r, 0.0, egui::Color32::from_gray(0x0a));
        }
    }

    // Ruler ticks: pick a step that gives ~80 px between labels.
    let px_per_s = rect.width() as f64 / span;
    let step = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0].into_iter().find(|s| s * px_per_s >= 80.0).unwrap_or(600.0);
    let mut t = (start / step).floor() * step;
    while t <= start + span {
        let x = x_of(t);
        painter.line_segment([egui::pos2(x, ruler.bottom() - 5.0), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0_f32, theme::RULE_SOFT));
        painter.line_segment([egui::pos2(x, ruler.bottom() - 5.0), egui::pos2(x, ruler.bottom())], egui::Stroke::new(1.0_f32, theme::RULE));
        painter.text(egui::pos2(x + 4.0, ruler.center().y - 1.0), egui::Align2::LEFT_CENTER, format_time(t), egui::FontId::monospace(9.5), theme::MUTED);
        t += step;
    }
    painter.line_segment([ruler.left_bottom(), ruler.right_bottom()], egui::Stroke::new(1.0_f32, theme::RULE));
    // Section separators.
    for (_, r) in &sections[1..] {
        painter.line_segment([r.left_top(), r.right_top()], egui::Stroke::new(1.0_f32, theme::RULE_SOFT));
    }

    // Waveform.
    if let Some(audio) = &app.audio {
        let color = egui::Color32::from_gray(0x8a);
        let mid = wave.center().y;
        let half = wave.height() * 0.45;
        let n = audio.peaks.len();
        for px in 0..rect.width() as usize {
            let t0 = t_of(rect.left() + px as f32);
            let t1 = t_of(rect.left() + px as f32 + 1.0);
            let (i0, i1) = ((t0 * PEAK_RATE as f64) as usize, ((t1 * PEAK_RATE as f64).ceil() as usize).max((t0 * PEAK_RATE as f64) as usize + 1));
            if i0 >= n {
                break;
            }
            let peak = audio.peaks[i0..i1.min(n)].iter().copied().fold(0.0, f32::max);
            let x = rect.left() + px as f32 + 0.5;
            let h = (peak * half).max(0.5);
            painter.line_segment([egui::pos2(x, mid - h), egui::pos2(x, mid + h)], egui::Stroke::new(1.0_f32, color));
        }
    } else {
        painter.text(wave.center(), egui::Align2::CENTER_CENTER, "No narration · File › Import narration or Transcribe", egui::FontId::proportional(11.0), theme::MUTED);
    }
    // The narration's clips: where the cuts are, and what can be cut.
    let segments = studio::narration_segments(app).unwrap_or_default();
    let can_cut = !segments.is_empty() && !app.studio.pending_sync;
    let audio_end = segments.last().map(|s| s.1).unwrap_or(0.0);
    for w in segments.windows(2) {
        let x = x_of(w[0].1);
        if x >= rect.left() && x <= rect.right() {
            painter.line_segment([egui::pos2(x, wave.top()), egui::pos2(x, wave.bottom())], egui::Stroke::new(1.0_f32, theme::FOCUS));
            // Notches at the top and bottom mark a cut.
            painter.add(egui::Shape::convex_polygon(vec![egui::pos2(x - 4.0, wave.top()), egui::pos2(x + 4.0, wave.top()), egui::pos2(x, wave.top() + 5.0)], theme::FOCUS, egui::Stroke::NONE));
            painter.add(egui::Shape::convex_polygon(vec![egui::pos2(x, wave.bottom() - 5.0), egui::pos2(x + 4.0, wave.bottom()), egui::pos2(x - 4.0, wave.bottom())], theme::FOCUS, egui::Stroke::NONE));
        }
    }

    // The Audio tab's tracks as clips on the same timeline (voice outlined, music filled).
    let row_h = clip_row - 3.0;
    for (row, (name, music, spans)) in strips.iter().enumerate().take(4) {
        let y = clips_lane.top() + 3.0 + row as f32 * clip_row;
        for &(a, b) in spans {
            let (x0, x1) = (x_of(a).max(rect.left()), x_of(b).min(rect.right()));
            if x1 <= x0 {
                continue;
            }
            let r = egui::Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + row_h));
            if *music {
                painter.rect_filled(r, 0.0, egui::Color32::from_gray(0x26));
            }
            painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, if *music { theme::RULE } else { egui::Color32::from_gray(0x5a) }), egui::StrokeKind::Inside);
        }
        if let Some(&(a, _)) = spans.first() {
            let x = x_of(a).max(rect.left()) + 3.0;
            painter.with_clip_rect(clips_lane).text(egui::pos2(x, y + row_h / 2.0), egui::Align2::LEFT_CENTER, name, egui::FontId::proportional((8.0 * fs).min(row_h)), theme::TEXT2);
        }
    }

    // Script lines.
    let mut hovered_line = None;
    if let Some(tl) = &app.parsed.timeline {
        for s in &tl.spans {
            let (x0, x1) = (x_of(s.start), x_of(s.end));
            if x1 < rect.left() || x0 > rect.right() {
                continue;
            }
            let r = egui::Rect::from_min_max(egui::pos2(x0 + 1.0, lines.top() + 3.0), egui::pos2(x1 - 1.0, lines.bottom() - 3.0));
            let now = s.start <= app.time && app.time < s.end;
            painter.rect_filled(r, 0.0, if now { egui::Color32::from_gray(0x14) } else { theme::BLACK });
            painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, if now { theme::FOCUS } else { theme::RULE }), egui::StrokeKind::Inside);
            let clip = painter.with_clip_rect(r.shrink(1.0).intersect(rect));
            clip.text(r.left_center() + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, &s.text, egui::FontId::proportional(10.5 * fs), if now { theme::WHITE } else { theme::TEXT2 });
            if response.hover_pos().is_some_and(|p| r.contains(p)) {
                hovered_line = Some((s.line_no, s.text.clone()));
            }
        }
    }

    // Caption words: when each word's highlight starts.
    let transcript: &[Word] = app.words.as_ref().map_or(&[], |w| &w.2);
    let mut line_words: Vec<LineWords> = app.parsed.timeline.as_ref().map(|tl| vg_core::captions::timed_words(tl, transcript)).unwrap_or_default();
    if let Some(d) = &app.timeline.word_drag
        && let Some(lw) = line_words.iter_mut().find(|l| l.span == d.span) {
            lw.words = d.words.clone();
        }
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    // (line index in `line_words`, word index, near the start edge)
    let mut word_hit: Option<(usize, usize, bool)> = None;
    let caption_on = app.project.preset.captions.enabled;
    for (li, lw) in line_words.iter().enumerate() {
        for (wi, w) in lw.words.iter().enumerate() {
            let end = lw.words.get(wi + 1).map(|n| n.start).unwrap_or(w.end);
            let (x0, x1) = (x_of(w.start), x_of(end));
            if x1 < rect.left() || x0 > rect.right() {
                continue;
            }
            let r = egui::Rect::from_min_max(egui::pos2(x0, words_lane.top() + 4.0), egui::pos2((x1 - 1.0).max(x0 + 1.0), words_lane.bottom() - 4.0));
            let now = caption_on && w.start <= app.time && app.time < end;
            let dragged = app.timeline.word_drag.as_ref().is_some_and(|d| d.span == lw.span && d.idx == wi);
            let hovered = pointer.is_some_and(|p| r.expand2(egui::vec2(3.0, 0.0)).contains(p) && words_lane.contains(p));
            painter.rect_filled(r, 0.0, if now { theme::WHITE } else if hovered || dragged { theme::HOVER } else { theme::BLACK });
            let edge = if dragged { theme::WHITE } else if hovered { theme::FOCUS } else { theme::RULE };
            if lw.from_transcript {
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
            } else {
                // Estimated timing: dashed until you pin it by dragging.
                theme::dashed_rect(&painter, r.shrink(0.5), egui::Stroke::new(1.0_f32, edge));
            }
            // The start edge is the handle.
            painter.line_segment([egui::pos2(x0, r.top()), egui::pos2(x0, r.bottom())], egui::Stroke::new(if hovered || dragged { 2.0_f32 } else { 1.0 }, if now { theme::WHITE } else { theme::BORDER }));
            if r.width() > 14.0 {
                let clip = painter.with_clip_rect(r.shrink(2.0).intersect(rect));
                clip.text(r.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, &w.word, egui::FontId::proportional(9.5 * fs), if now { theme::BLACK } else if lw.from_transcript { theme::TEXT2 } else { theme::MUTED });
            }
            if let Some(p) = pointer
                && words_lane.contains(p) && p.x >= x0 - 4.0 && p.x < x1.max(x0 + 6.0) {
                    let near_edge = (p.x - x0).abs() <= 5.0 || r.width() < 12.0;
                    // Prefer the word whose edge is under the pointer.
                    if word_hit.is_none() || near_edge {
                        word_hit = Some((li, wi, near_edge));
                    }
                }
        }
    }
    if line_words.is_empty() && app.parsed.timeline.is_some() {
        painter.text(words_lane.center(), egui::Align2::CENTER_CENTER, "No words · write or transcribe some lines", egui::FontId::proportional(10.0), theme::MUTED);
    }

    let clips = app.project.video.clips.clone();
    // Thumbnails load in the background (a frame of video, the picture for stills).
    let looks: Vec<(Option<(egui::TextureId, egui::Vec2)>, bool)> = clips
        .iter()
        .map(|c| {
            let path = video::source_path(app, c);
            if path.exists() { (media::thumb(app, &ctx, &path), false) } else { (None, true) }
        })
        .collect();

    // A video file dragged from the Media tab: its length, for the drop ghost.
    let hover_len = response
        .dnd_hover_payload::<video::MediaDrop>()
        .filter(|d| media::kind_of(&d.0) == Some(media::Kind::Video))
        .and_then(|d| drop_len(app, &ctx, &d.0));

    // Snap targets, by mode. Captions: word transitions, line cuts, pose
    // changes. Video: clip edges. Both: the start and the playhead.
    let mode = app.prefs.snap_timeline.then_some(app.prefs.snap_to);
    let snap_on = mode.is_some() && !ui.input(|i| i.modifiers.alt);
    let snap_radius = 8.0 / px_per_s;
    let vedges = app.project.video.edges();
    let targets = |skip_word: Option<(usize, usize)>, skip_event: Option<usize>| -> Vec<f64> {
        let mut v = vec![0.0, app.time];
        if mode == Some(video::SnapTo::Video) {
            v.extend(&vedges);
            return v;
        }
        for lw in &line_words {
            for (wi, w) in lw.words.iter().enumerate() {
                if skip_word == Some((lw.span, wi)) {
                    continue;
                }
                v.push(w.start);
                v.push(w.end);
            }
        }
        if let Some(tl) = &app.parsed.timeline {
            for s in &tl.spans {
                v.push(s.start);
                v.push(s.end);
            }
            for (i, e) in tl.events.iter().enumerate() {
                if Some(i) != skip_event {
                    v.push(e.t);
                }
            }
        }
        v
    };

    // ---- the video track ----
    let dragging_clip = app.timeline.vdrag.as_ref().map(|d| (d.i, d.grab, d.before.clips.get(d.i).cloned()));
    let block_of = |c: &vg_project::video::VideoClip| egui::Rect::from_min_max(egui::pos2(x_of(c.start), vlane.top() + 3.0), egui::pos2(x_of(c.end()).max(x_of(c.start) + 2.0), vlane.bottom() - 3.0));
    let fade_x = |c: &vg_project::video::VideoClip, r: egui::Rect| ((x_of(c.start + c.fade_in)).max(r.left() + 4.0), (x_of(c.end() - c.fade_out)).min(r.right() - 4.0));
    // What the pointer (or the press that started a drag) is over.
    let at = if response.drag_started() { ui.input(|i| i.pointer.press_origin()).or(pointer) } else { pointer };
    let vhit: Option<(usize, Grab)> = at.filter(|p| vlane.contains(*p)).and_then(|p| {
        clips.iter().enumerate().find_map(|(i, c)| {
            let r = block_of(c);
            let (fi, fo) = fade_x(c, r);
            r.expand2(egui::vec2(3.0, 3.0)).contains(p).then(|| (i, grab_at(p.x, p.y, r.left(), r.right(), r.top(), fi, fo)))
        })
    });
    let hover_clip = vhit.map(|h| h.0).filter(|_| app.timeline.vdrag.is_none());
    let vsel_hot = app.timeline.vsel_drag.is_some() || (app.timeline.vdrag.is_none() && at.is_some_and(|p| vlane.contains(p)) && (vhit.is_none() || ui.input(|i| i.modifiers.shift)));
    for (i, c) in clips.iter().enumerate() {
        let r = block_of(c);
        if r.right() < rect.left() || r.left() > rect.right() {
            continue;
        }
        let (thumb, missing) = looks[i];
        let selected = app.video.selected == Some(i);
        let hovered = hover_clip == Some(i) || dragging_clip.as_ref().is_some_and(|d| d.0 == i);
        let inner = r.intersect(rect);
        let clip_p = painter.with_clip_rect(inner);
        clip_p.rect_filled(r, 0.0, if missing { theme::DANGER.gamma_multiply(0.18) } else { egui::Color32::from_gray(0x1a) });
        // A filmstrip: the thumbnail repeated along the clip, anchored to its start.
        if let Some((tex, size)) = thumb {
            let tile_w = (r.height() * size.x / size.y.max(1.0)).clamp(8.0, 400.0);
            let first = ((inner.left() - r.left()) / tile_w).floor().max(0.0) as usize;
            let tint = egui::Color32::from_gray(if selected || hovered { 0xc0 } else { 0x90 });
            for n in first..first + 200 {
                let x = r.left() + n as f32 * tile_w;
                if x > inner.right() {
                    break;
                }
                let tile = egui::Rect::from_min_size(egui::pos2(x, r.top()), egui::vec2(tile_w - 1.0, r.height()));
                clip_p.image(tex, tile, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
            }
        }
        // Fades: the corner that fades is shaded, with a ramp line.
        let (fi, fo) = (x_of(c.start + c.fade_in), x_of(c.end() - c.fade_out));
        let shade = egui::Color32::from_black_alpha(170);
        let ramp = egui::Stroke::new(1.0_f32, theme::FOCUS);
        if c.fade_in > 0.0 {
            clip_p.add(egui::Shape::convex_polygon(vec![r.left_top(), egui::pos2(fi, r.top()), r.left_bottom()], shade, egui::Stroke::NONE));
            clip_p.line_segment([r.left_bottom(), egui::pos2(fi, r.top())], ramp);
        }
        if c.fade_out > 0.0 {
            clip_p.add(egui::Shape::convex_polygon(vec![egui::pos2(fo, r.top()), r.right_top(), r.right_bottom()], shade, egui::Stroke::NONE));
            clip_p.line_segment([egui::pos2(fo, r.top()), r.right_bottom()], ramp);
        }
        // The name, on a dark chip so it reads over the pictures; it stays in view when scrolled.
        if inner.width() > 26.0 {
            let name = if missing { format!("{} · missing", clip_name(c)) } else { clip_name(c) };
            let g = painter.layout_no_wrap(name, egui::FontId::proportional(9.5 * fs), if missing { theme::DANGER } else if selected { theme::WHITE } else { theme::TEXT2 });
            let pos = egui::pos2(inner.left() + 5.0, r.top() + 3.0);
            let chip = egui::Rect::from_min_size(pos - egui::vec2(3.0, 1.0), g.size() + egui::vec2(6.0, 2.0));
            let text_p = painter.with_clip_rect(inner.shrink2(egui::vec2(1.0, 0.0)));
            text_p.rect_filled(chip, 0.0, egui::Color32::from_black_alpha(190));
            text_p.galley(pos, g, theme::TEXT2);
        }
        // Markers, bottom right: a still image, a muted clip.
        let mut mx = inner.right() - 5.0;
        if c.muted && inner.width() > 30.0 {
            let m = egui::pos2(mx - 6.0, r.bottom() - 7.0);
            clip_p.rect_filled(egui::Rect::from_center_size(m, egui::vec2(15.0, 11.0)), 0.0, egui::Color32::from_black_alpha(190));
            muted_glyph(&clip_p, m, theme::WHITE);
            mx -= 17.0;
        }
        if c.still && inner.width() > 50.0 {
            let g = painter.layout_no_wrap("STILL".into(), egui::FontId::monospace(8.0), theme::TEXT2);
            let chip = egui::Rect::from_min_size(egui::pos2(mx - g.size().x - 6.0, r.bottom() - g.size().y - 4.0), g.size() + egui::vec2(6.0, 2.0));
            clip_p.rect_filled(chip, 0.0, egui::Color32::from_black_alpha(190));
            clip_p.galley(chip.min + egui::vec2(3.0, 1.0), g, theme::TEXT2);
        }
        // The edge: selected white, hovered lit, missing red.
        let (w, col) = if missing {
            (1.0_f32, theme::DANGER)
        } else if selected {
            (1.5, theme::WHITE)
        } else if hovered {
            (1.0, theme::FOCUS)
        } else {
            (1.0, theme::BORDER)
        };
        painter.rect_stroke(r, 0.0, egui::Stroke::new(w, col), egui::StrokeKind::Inside);
        // Fade handles: small triangles at the top corners (where the fade ends).
        if selected || hovered || c.fade_in > 0.0 || c.fade_out > 0.0 {
            let (hi, ho) = fade_x(c, r);
            for (x, g) in [(hi, Grab::FadeIn), (ho, Grab::FadeOut)] {
                let hot = vhit == Some((i, g)) || dragging_clip.as_ref().is_some_and(|d| d.0 == i && d.1 == g);
                let s = if hot { 5.5 } else { 4.0 };
                let tri = vec![egui::pos2(x - s, r.top()), egui::pos2(x + s, r.top()), egui::pos2(x, r.top() + s * 1.3)];
                painter.add(egui::Shape::convex_polygon(tri, if hot { theme::WHITE } else { theme::FOCUS }, egui::Stroke::new(1.0_f32, theme::BLACK)));
            }
        }
    }
    if clips.is_empty() {
        painter.text(vlane.center(), egui::Align2::CENTER_CENTER, "Add video from the Media tab · drag a card here", egui::FontId::proportional(10.5), theme::MUTED);
    }
    // A drag in progress: the clip as it was (dashed), and what's changing.
    if let Some((i, grab, Some(was))) = &dragging_clip
        && let Some(c) = clips.get(*i) {
            let (r0, r) = (block_of(was), block_of(c));
            theme::dashed_rect(&painter, r0.shrink(0.5), egui::Stroke::new(1.0_f32, theme::MUTED));
            let (text, x) = match grab {
                Grab::Move => (format!("{} → {}", format_time(c.start), format_time(c.end())), r.center().x),
                Grab::Start => (format!("{:.2} s", c.len()), r.left()),
                Grab::End => (format!("{:.2} s", c.len()), r.right()),
                Grab::FadeIn => (format!("fade in {:.2} s", c.fade_in), fade_x(c, r).0),
                Grab::FadeOut => (format!("fade out {:.2} s", c.fade_out), fade_x(c, r).1),
            };
            tag(&painter, egui::pos2(x.clamp(rect.left() + 40.0, rect.right() - 40.0), r.center().y + 2.0), &text, theme::WHITE);
        }

    // Pose markers.
    let mut marker_hit: Option<usize> = None;
    // Flags that would overlap the previous one drop to a second row.
    let mut row_end = [f32::NEG_INFINITY; 2];
    if let Some(tl) = &app.parsed.timeline {
        for (i, ev) in tl.events.iter().enumerate() {
            let t = match app.timeline.dragging {
                Some((d, dt)) if d == i => dt,
                _ => ev.t,
            };
            let x = x_of(t);
            if x < rect.left() - 60.0 || x > rect.right() {
                continue;
            }
            let label = tag_name(app, ev).to_uppercase();
            let color = if matches!(ev.action, Action::Pose(_)) { theme::MARK } else { theme::WHITE };
            let head_w = painter.layout_no_wrap(label.clone(), egui::FontId::proportional(10.0 * fs), color).size().x + 12.0 * fs;
            let row = if x >= row_end[0] + 2.0 { 0 } else if x >= row_end[1] + 2.0 { 1 } else { 0 };
            row_end[row] = x + head_w;
            let head = egui::Rect::from_min_size(egui::pos2(x, marks.top() + 5.0 * fs + row as f32 * 20.0 * fs), egui::vec2(head_w, 17.0 * fs));
            let hot = pointer.is_some_and(|p| head.expand(2.0).contains(p)) || app.timeline.dragging.is_some_and(|(d, _)| d == i);
            let stem = egui::Stroke::new(1.0_f32, color.gamma_multiply(if hot { 0.9 } else { 0.45 }));
            if matches!(ev.action, Action::Pose(_)) {
                painter.line_segment([egui::pos2(x, ruler.bottom()), egui::pos2(x, head.bottom())], stem);
            } else {
                painter.extend(egui::Shape::dashed_line(&[egui::pos2(x, ruler.bottom()), egui::pos2(x, head.bottom())], stem, 3.0, 3.0));
            }
            painter.rect_filled(head, 0.0, if hot { color } else { theme::BLACK });
            painter.rect_stroke(head, 0.0, egui::Stroke::new(1.0_f32, color), egui::StrokeKind::Inside);
            painter.text(head.left_center() + egui::vec2(6.0 * fs, 0.0), egui::Align2::LEFT_CENTER, &label, egui::FontId::proportional(10.0 * fs), if hot { theme::BLACK } else { color });
            // A pinned (@time) tag has a solid foot; a tag placed by its text position, a hollow one.
            let foot = egui::Rect::from_center_size(egui::pos2(x, marks.bottom() - 6.0 * kl.min(1.5)), egui::vec2(5.0 * fs, 5.0 * fs));
            if ev.timed {
                painter.rect_filled(foot, 0.0, color);
            } else {
                painter.rect_stroke(foot, 0.0, egui::Stroke::new(1.0_f32, color), egui::StrokeKind::Inside);
            }
            if pointer.is_some_and(|p| head.expand(2.0).contains(p)) {
                marker_hit = Some(i);
            }
        }
    }

    // The end of the video: everything after it is shaded; its handle drags.
    let end_t = app.timeline.end_drag.unwrap_or(duration);
    let ex = x_of(end_t);
    let label = format!("END {}", format_time(end_t));
    let tab = egui::Rect::from_min_size(egui::pos2(ex - 2.0 - label.len() as f32 * 6.0 - 10.0, ruler.top() + 2.0), egui::vec2(label.len() as f32 * 6.0 + 10.0, ruler.height() - 4.0));
    let end_hot = pointer.is_some_and(|p| tab.expand(2.0).contains(p) || ((p.x - ex).abs() <= 6.0 && p.y <= ruler.bottom() + 4.0)) || app.timeline.end_drag.is_some();
    if ex < rect.right() {
        let past = egui::Rect::from_min_max(egui::pos2(ex.max(rect.left()), ruler.bottom()), rect.right_bottom());
        painter.rect_filled(past, 0.0, egui::Color32::from_black_alpha(170));
    }
    if ex >= rect.left() && ex <= rect.right() {
        painter.line_segment([egui::pos2(ex, ruler.top()), egui::pos2(ex, rect.bottom())], egui::Stroke::new(1.0_f32, if end_hot { theme::WHITE } else { theme::FOCUS }));
        painter.rect_filled(tab, 0.0, if end_hot { theme::WHITE } else { theme::BLACK });
        painter.rect_stroke(tab, 0.0, egui::Stroke::new(1.0_f32, theme::FOCUS), egui::StrokeKind::Inside);
        painter.text(tab.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::monospace(9.0), if end_hot { theme::BLACK } else { theme::WHITE });
    }
    if end_hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }

    // The narration selection, and a trim in progress (what it will remove).
    // A cut moves the script and words too, so it shades those sections, not the video.
    let cut_band = |x0: f32, x1: f32| egui::Rect::from_min_max(egui::pos2(x0, wave.top()), egui::pos2(x1, words_lane.bottom()));
    if let Some((a, b)) = app.timeline.sel {
        let (x0, x1) = (x_of(a).max(rect.left()), x_of(b).min(rect.right()));
        if x1 > x0 {
            painter.rect_filled(cut_band(x0, x1), 0.0, egui::Color32::from_white_alpha(14));
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, wave.top()), egui::pos2(x1, wave.bottom())), 0.0, egui::Color32::from_white_alpha(20));
            for x in [x_of(a), x_of(b)] {
                if x >= rect.left() && x <= rect.right() {
                    painter.line_segment([egui::pos2(x, wave.top()), egui::pos2(x, words_lane.bottom())], egui::Stroke::new(1.0_f32, theme::WHITE));
                }
            }
            let g = painter.layout_no_wrap(format_time(b - a), egui::FontId::monospace(9.5), theme::BLACK);
            if g.size().x + 10.0 < x1 - x0 {
                tag(&painter, egui::pos2((x0 + x1) / 2.0, wave.top() + 10.0), &format_time(b - a), theme::WHITE);
            }
        }
    }
    if let Some(tr) = app.timeline.trim {
        let (a, b) = (tr.at.min(tr.t), tr.at.max(tr.t));
        let r = cut_band(x_of(a), x_of(b)).intersect(rect);
        painter.rect_filled(r, 0.0, theme::DANGER.gamma_multiply(0.22));
        painter.line_segment([egui::pos2(x_of(tr.t), wave.top()), egui::pos2(x_of(tr.t), words_lane.bottom())], egui::Stroke::new(1.5_f32, theme::DANGER));
        if b - a > 0.0 {
            painter.text(egui::pos2(r.center().x, wave.top() + 10.0), egui::Align2::CENTER_CENTER, format!("−{}", format_time(b - a)), egui::FontId::monospace(9.5), theme::DANGER);
        }
    }
    // The video selection, in the Video section.
    if let Some((a, b)) = app.timeline.vsel {
        let (x0, x1) = (x_of(a).max(rect.left()), x_of(b).min(rect.right()));
        if x1 > x0 {
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, vlane.top()), egui::pos2(x1, vlane.bottom())), 0.0, egui::Color32::from_white_alpha(36));
            for x in [x_of(a), x_of(b)] {
                if x >= rect.left() && x <= rect.right() {
                    painter.line_segment([egui::pos2(x, vlane.top()), egui::pos2(x, vlane.bottom())], egui::Stroke::new(1.0_f32, theme::WHITE));
                }
            }
            let text = format_time(b - a);
            if (text.len() as f32) * 6.0 + 14.0 < x1 - x0 {
                tag(&painter, egui::pos2((x0 + x1) / 2.0, vlane.center().y), &text, theme::WHITE);
            }
        }
    }

    // Media dragged from the Media tab: a ghost where it would land, in its
    // kind's section (video and pictures on the video track, audio under the voice).
    let dnd_pos = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.contains(*p));
    let hover_drop = response.dnd_hover_payload::<video::MediaDrop>().filter(|_| dnd_pos.is_some());
    let drop_t = dnd_pos.map(|p| {
        let raw = t_of(p.x).max(0.0);
        let mut v = targets(None, None);
        if mode.is_some() {
            v.extend(&vedges);
        }
        if snap_on { snap(raw, &v, snap_radius) } else { (raw, None) }
    });
    let mut drop_lane = None;
    if let (Some(d), Some((t, hit))) = (&hover_drop, drop_t) {
        let kind = media::kind_of(&d.0);
        let audio = kind == Some(media::Kind::Audio);
        drop_lane = Some(if audio { Lane::Audio } else { Lane::Video });
        let name = file_name(&d.0);
        let (at, len) = if audio {
            (t, STILL_SECONDS.min(span * 0.25))
        } else {
            // Where it lands: the first gap at or after `t` it fits in.
            let still = kind == Some(media::Kind::Image);
            let len = if still { None } else { hover_len };
            let mut track = app.project.video.clone();
            let i = track.insert(vg_project::video::VideoClip::new(d.0.clone(), t, len, still), t);
            (track.clips[i].start, track.clips[i].len())
        };
        let band = if audio { egui::Rect::from_min_max(egui::pos2(0.0, (audio_sec.bottom() - 16.0).max(audio_sec.top())), egui::pos2(0.0, audio_sec.bottom() - 2.0)) } else { vlane.shrink2(egui::vec2(0.0, 3.0)) };
        let ghost = egui::Rect::from_min_max(egui::pos2(x_of(at), band.top()), egui::pos2(x_of(at + len), band.bottom()));
        painter.rect_filled(ghost, 0.0, egui::Color32::from_white_alpha(40));
        theme::dashed_rect(&painter, ghost, egui::Stroke::new(1.0_f32, theme::WHITE));
        painter.line_segment([egui::pos2(ghost.left(), ruler.bottom()), egui::pos2(ghost.left(), rect.bottom())], egui::Stroke::new(1.5_f32, theme::WHITE));
        painter.with_clip_rect(ghost.intersect(rect)).text(ghost.left_center() + egui::vec2(5.0, 0.0), egui::Align2::LEFT_CENTER, format!("+ {name}"), egui::FontId::proportional(9.5 * fs), theme::WHITE);
        // No room at the pointer: an arrow from there to the gap it'll take.
        if at > t + 1e-6 {
            let (x0, y) = (x_of(t), band.center().y);
            painter.extend(egui::Shape::dashed_line(&[egui::pos2(x0, y), egui::pos2(ghost.left() - 2.0, y)], egui::Stroke::new(1.0_f32, theme::WHITE), 3.0, 3.0));
            painter.add(egui::Shape::convex_polygon(vec![egui::pos2(ghost.left() - 1.0, y), egui::pos2(ghost.left() - 6.0, y - 3.5), egui::pos2(ghost.left() - 6.0, y + 3.5)], theme::WHITE, egui::Stroke::NONE));
        }
        tag(&painter, egui::pos2((ghost.left() + 30.0).min(rect.right() - 30.0), ruler.center().y), &format_time(at), theme::WHITE);
        app.timeline.snapped = hit.filter(|h| (h - at).abs() < 1e-6);
    }
    // Added once this frame's drawing is done.
    let dropped = response.dnd_release_payload::<video::MediaDrop>().zip(drop_t).map(|(d, (t, _))| (d.0.clone(), t));

    // Playhead.
    let px = x_of(app.time);
    if px >= rect.left() && px <= rect.right() {
        theme::glow_line(&painter, egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom()), theme::WHITE);
        let tri = vec![egui::pos2(px - 5.0, rect.top()), egui::pos2(px + 5.0, rect.top()), egui::pos2(px, rect.top() + 6.0)];
        painter.add(egui::Shape::convex_polygon(tri, theme::WHITE, egui::Stroke::NONE));
    }

    // The snap guide.
    if let Some(t) = app.timeline.snapped {
        let x = x_of(t);
        painter.extend(egui::Shape::dashed_line(&[egui::pos2(x, ruler.bottom()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0_f32, theme::WHITE), 4.0, 3.0));
    }

    // Cutting in the audio lane: selection edges, clip edges (trim), or a new selection.
    // A drag is only reported once the pointer has moved: judge it from where it was pressed.
    let in_wave = can_cut && at.is_some_and(|p| wave.contains(p)) && marker_hit.is_none();
    let in_video = at.is_some_and(|p| vlane.contains(p)) && marker_hit.is_none();
    let near = |t: f64| at.is_some_and(|p| (x_of(t) - p.x).abs() <= 5.0);
    let sel_edge_hit = app.timeline.sel.filter(|_| in_wave).and_then(|(a, b)| if near(b) { Some(true) } else if near(a) { Some(false) } else { None });
    let trim_hit = if in_wave && sel_edge_hit.is_none() {
        let mut hit = None;
        for (i, &(s, e)) in segments.iter().enumerate() {
            if near(s) {
                let lo = if i > 0 { segments[i - 1].0 + 0.05 } else { s };
                hit = Some(Trim { at: s, lo, hi: e - 0.05, t: s });
            } else if i + 1 == segments.len() && near(e) {
                hit = Some(Trim { at: e, lo: s + 0.05, hi: e, t: e });
            }
        }
        hit
    } else {
        None
    };
    let cut_targets = || {
        let mut v = targets(None, None);
        v.extend(segments.iter().flat_map(|s| [s.0, s.1]));
        v
    };
    // Video drags snap to the mode's targets plus the other clips' edges.
    let video_targets = |track: &VideoTrack, skip: Option<usize>| -> Vec<f64> {
        let mut v = targets(None, None);
        for (i, c) in track.clips.iter().enumerate() {
            if Some(i) != skip {
                v.push(c.start);
                v.push(c.end());
            }
        }
        v
    };

    // ---- interaction ----
    // A press sets which section the keys act on.
    if (response.drag_started() || response.clicked() || response.secondary_clicked())
        && let Some(p) = at {
            if audio_sec.contains(p) {
                app.timeline.focus = Focus::Audio;
            } else if vlane.contains(p) {
                app.timeline.focus = Focus::Video;
            }
        }
    if gutter_resp.clicked()
        && let Some(p) = gutter_resp.interact_pointer_pos() {
            if p.y >= audio_sec.top() && p.y < audio_sec.bottom() {
                app.timeline.focus = Focus::Audio;
            } else if p.y >= vlane.top() && p.y < vlane.bottom() {
                app.timeline.focus = Focus::Video;
            }
        }
    if response.drag_started() && end_hot {
        app.timeline.end_drag = Some(duration);
    } else if response.drag_started() && in_wave {
        if let Some(e) = sel_edge_hit {
            app.timeline.sel_drag = Some(SelDrag::Edge(e));
        } else if let Some(tr) = trim_hit {
            app.timeline.trim = Some(tr);
        } else if let Some(p) = at {
            let raw = t_of(p.x).clamp(0.0, audio_end);
            let t = if snap_on { snap(raw, &cut_targets(), snap_radius).0 } else { raw };
            app.timeline.sel_drag = Some(SelDrag::Range(t.clamp(0.0, audio_end)));
            app.timeline.sel = None;
        }
    } else if response.drag_started() && in_video {
        let shift = ui.input(|i| i.modifiers.shift);
        match vhit.filter(|_| !shift) {
            // Grab a clip: move, trim or fade it.
            Some((i, grab)) => {
                let c = &app.project.video.clips[i];
                let offset = at.map(|p| t_of(p.x)).unwrap_or(c.start) - c.start;
                app.video.selected = Some(i);
                app.timeline.vsel = None;
                app.timeline.vdrag = Some(VideoDrag { i, grab, offset, before: app.project.video.clone() });
            }
            // A gap (or Shift): select a stretch of the track.
            None => {
                if let Some(p) = at {
                    let raw = t_of(p.x).max(0.0);
                    let t = if snap_on { snap(raw, &video_targets(&app.project.video, None), snap_radius).0 } else { raw };
                    app.timeline.vsel_drag = Some(t.max(0.0));
                    app.timeline.vsel = None;
                    app.video.selected = None;
                }
            }
        }
    } else if response.drag_started() {
        if let Some(i) = marker_hit {
            let t = app.parsed.timeline.as_ref().map(|tl| tl.events[i].t).unwrap_or(0.0);
            app.timeline.dragging = Some((i, t));
        } else if let (Some((li, wi, edge)), Some(p)) = (word_hit, response.interact_pointer_pos()) {
            let lw = &line_words[li];
            app.timeline.word_drag = Some(WordDrag { span: lw.span, idx: wi, body: !edge, grab: t_of(p.x) - lw.words[wi].start, words: lw.words.clone() });
        }
    }
    if hover_drop.is_none() {
        app.timeline.snapped = None;
    }
    if app.timeline.end_drag.is_some() {
        if let Some(p) = response.interact_pointer_pos() {
            let raw = t_of(p.x).max(0.5);
            let mut ends = targets(None, None);
            ends.extend(audio_len);
            let (t, hit) = if snap_on { snap(raw, &ends, snap_radius) } else { (round2(raw), None) };
            app.timeline.end_drag = Some(t.max(0.5));
            app.timeline.snapped = hit;
        }
        if response.drag_stopped() {
            let t = app.timeline.end_drag.take().unwrap();
            set_length(app, t);
        }
    } else if let Some(tr) = app.timeline.trim {
        if let Some(p) = response.interact_pointer_pos() {
            let raw = t_of(p.x);
            let (t, hit) = if snap_on { snap(raw, &cut_targets(), snap_radius) } else { (raw, None) };
            app.timeline.trim = Some(Trim { t: t.clamp(tr.lo, tr.hi), ..tr });
            app.timeline.snapped = hit;
        }
        if response.drag_stopped() {
            let tr = app.timeline.trim.take().unwrap();
            if (tr.t - tr.at).abs() >= 0.01 {
                cut(app, &ctx, studio::Cut::Delete(tr.at, tr.t));
            }
        }
    } else if let Some(sd) = app.timeline.sel_drag {
        if let Some(p) = response.interact_pointer_pos() {
            let raw = t_of(p.x).clamp(0.0, audio_end);
            let (t, hit) = if snap_on { snap(raw, &cut_targets(), snap_radius) } else { (raw, None) };
            let t = t.clamp(0.0, audio_end);
            app.timeline.sel = match (sd, app.timeline.sel) {
                (SelDrag::Range(anchor), _) => Some((anchor.min(t), anchor.max(t))),
                (SelDrag::Edge(end), Some((a, b))) => {
                    let (a, b) = if end { (a, t) } else { (t, b) };
                    Some((a.min(b), a.max(b)))
                }
                (_, s) => s,
            };
            app.timeline.snapped = hit;
        }
        if response.drag_stopped() {
            app.timeline.sel_drag = None;
            if app.timeline.sel.is_some_and(|(a, b)| x_of(b) - x_of(a) < 3.0) {
                app.timeline.sel = None;
            }
        }
    } else if let Some((i, grab, offset, before)) = app.timeline.vdrag.as_ref().map(|d| (d.i, d.grab, d.offset, d.before.clone())) {
        // Re-apply the drag to the track as it was, so the preview shows it live.
        if let Some(p) = response.interact_pointer_pos() {
            let c = before.clips[i].clone();
            let pt = t_of(p.x);
            let v = if snap_on { video_targets(&before, Some(i)) } else { vec![] };
            let mut tr = before;
            let hit = match grab {
                Grab::Move => {
                    let (s, hit) = snap_span(pt - offset, c.len(), &v, snap_radius);
                    let got = tr.move_to(i, if hit.is_some() { s } else { round2(s) });
                    hit.filter(|h| (got - h).abs() < 1e-6 || (got + c.len() - h).abs() < 1e-6)
                }
                Grab::Start | Grab::End => {
                    let (t, hit) = snap(pt, &v, snap_radius);
                    let t = if hit.is_some() { t } else { round2(t) };
                    let got = if grab == Grab::Start { tr.trim_start(i, t) } else { tr.trim_end(i, t) };
                    hit.filter(|h| (got - h).abs() < 1e-6)
                }
                Grab::FadeIn => {
                    tr.set_fades(i, round2(pt - c.start).max(0.0), c.fade_out);
                    None
                }
                Grab::FadeOut => {
                    tr.set_fades(i, c.fade_in, round2(c.end() - pt).max(0.0));
                    None
                }
            };
            app.project.video = tr;
            app.timeline.snapped = hit;
        }
        if response.drag_stopped() {
            let d = app.timeline.vdrag.take().unwrap();
            if video::commit(app, d.before) {
                let c = &app.project.video.clips[d.i];
                let what = match d.grab {
                    Grab::Move => format!("moved the clip to {}", format_time(c.start)),
                    Grab::Start | Grab::End => format!("trimmed the clip to {}", format_time(c.len())),
                    Grab::FadeIn | Grab::FadeOut => format!("fades {:.2} s in, {:.2} s out", c.fade_in, c.fade_out),
                };
                app.info(format!("{what} · Ctrl+Z undoes it"));
            }
        }
    } else if let Some(anchor) = app.timeline.vsel_drag {
        if let Some(p) = response.interact_pointer_pos() {
            let raw = t_of(p.x).max(0.0);
            let (t, hit) = if snap_on { snap(raw, &video_targets(&app.project.video, None), snap_radius) } else { (raw, None) };
            app.timeline.vsel = Some((anchor.min(t), anchor.max(t)));
            app.timeline.snapped = hit;
        }
        if response.drag_stopped() {
            app.timeline.vsel_drag = None;
            if app.timeline.vsel.is_some_and(|(a, b)| x_of(b) - x_of(a) < 3.0) {
                app.timeline.vsel = None;
            }
        }
    } else if let Some(d) = &mut app.timeline.word_drag {
        if let Some(p) = response.interact_pointer_pos() {
            let raw = if d.body { t_of(p.x) - d.grab } else { t_of(p.x) };
            // Keep word order: stay between the neighbours' starts, and only snap to what's reachable.
            let lo = if d.idx > 0 { d.words[d.idx - 1].start + 0.03 } else { f64::NEG_INFINITY };
            let hi = d.words.get(d.idx + 1).map(|n| n.start - 0.03).unwrap_or(f64::INFINITY);
            let reachable: Vec<f64> = targets(Some((d.span, d.idx)), None).into_iter().filter(|t| *t >= lo && *t <= hi).collect();
            let (mut t, hit) = if snap_on { snap(raw, &reachable, snap_radius) } else { (round2(raw), None) };
            t = t.clamp(lo.min(hi), hi.max(lo));
            let w = &mut d.words[d.idx];
            if d.body {
                let len = w.end - w.start;
                w.start = t;
                w.end = t + len;
            } else {
                w.start = t;
                w.end = w.end.max(t + 0.05);
            }
            if d.idx > 0 && d.words[d.idx - 1].end > t {
                d.words[d.idx - 1].end = t;
            }
            app.timeline.snapped = hit;
        }
        if response.drag_stopped() {
            let d = app.timeline.word_drag.take().unwrap();
            app.set_line_words(d.span, &d.words);
        }
    } else if let Some((i, _)) = app.timeline.dragging {
        if let Some(p) = response.interact_pointer_pos() {
            let raw = t_of(p.x);
            let (t, hit) = if snap_on { snap(raw, &targets(None, Some(i)), snap_radius) } else { (round2(raw), None) };
            app.timeline.dragging = Some((i, t.clamp(0.0, duration)));
            app.timeline.snapped = hit;
        }
        if response.drag_stopped() {
            let (i, t) = app.timeline.dragging.take().unwrap();
            if let Some(ev) = app.parsed.timeline.as_ref().map(|tl| tl.events[i].clone()) {
                let name = tag_name(app, &ev);
                if script::retime_tag(&mut app.cue_text, ev.line_no, ev.range, &name, t) {
                    app.reparse();
                }
            }
        }
    } else if response.clicked() && in_wave {
        // Click a clip to select it.
        if let Some(p) = response.interact_pointer_pos() {
            let t = t_of(p.x);
            app.timeline.sel = segments.iter().copied().find(|s| t >= s.0 && t < s.1);
            app.seek(t);
        }
    } else if response.clicked() && in_video {
        // Click a video clip to select it (a gap drops the selection).
        if let Some(p) = response.interact_pointer_pos() {
            let t = t_of(p.x);
            app.video.selected = vhit.map(|h| h.0);
            app.timeline.vsel = None;
            app.seek(t.max(0.0));
        }
    } else if (response.clicked() || response.dragged())
        && let Some(p) = response.interact_pointer_pos() {
            if response.clicked() && let Some(i) = marker_hit {
                let ev = &app.parsed.timeline.as_ref().unwrap().events[i];
                let (t, line) = (ev.t, ev.line_no);
                app.seek(t);
                app.script.jump_to_line(line);
            } else if response.clicked() && lines.contains(p) && let Some((line, _)) = &hovered_line {
                app.script.jump_to_line(*line);
                app.seek(t_of(p.x));
            } else {
                app.seek(t_of(p.x));
            }
        }
    if response.double_clicked() && end_hot && app.parsed.sheet.as_ref().is_some_and(|s| s.duration.is_some()) {
        super::remove_directive(&mut app.cue_text, "duration");
        app.reparse();
        app.info("the video ends with the narration again");
    }
    // Right-click: a flag is deleted; a clip gets its menu.
    let menu_for = if response.secondary_clicked() && marker_hit.is_none() { vhit.map(|h| h.0) } else { None };
    if response.secondary_clicked() {
        if let Some(i) = marker_hit {
            let ev = app.parsed.timeline.as_ref().unwrap().events[i].clone();
            if script::delete_tag(&mut app.cue_text, ev.line_no, ev.range) {
                app.reparse();
            }
        } else if let Some(i) = menu_for {
            app.video.selected = Some(i);
            app.timeline.vsel = None;
            app.timeline.menu_clip = Some(i);
        }
    }
    clip_menu(app, &response, menu_for.is_some());

    if let Some((li, wi, edge)) = word_hit.filter(|_| marker_hit.is_none() && app.timeline.dragging.is_none()) {
        let lw = &line_words[li];
        let w = &lw.words[wi];
        ui.ctx().set_cursor_icon(if edge || app.timeline.word_drag.as_ref().is_some_and(|d| !d.body) { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::Grab });
        if app.timeline.word_drag.is_none() {
            let src = if lw.from_transcript { "from the transcript" } else { "estimated (drag to pin)" };
            response.clone().on_hover_text(format!("{} · highlight from {} · {src}\nDrag the left edge to retime · drag the word to move it", w.word, format_time(w.start)));
        }
    } else if let Some(i) = marker_hit {
        if app.timeline.dragging.is_none() {
            let ev = &app.parsed.timeline.as_ref().unwrap().events[i];
            response.clone().on_hover_text(format!("{} at {} (line {})\nDrag to retime · right-click to delete", tag_name(app, ev), format_time(ev.t), ev.line_no));
        }
    } else if let Some((_, text)) = &hovered_line {
        response.clone().on_hover_text(text);
    }

    if sel_edge_hit.is_some() || trim_hit.is_some() || app.timeline.trim.is_some() || matches!(app.timeline.sel_drag, Some(SelDrag::Edge(_))) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    // Video cursors: resize on edges and fade handles, grab on a clip's body.
    let vgrab = app.timeline.vdrag.as_ref().map(|d| d.grab).or(vhit.filter(|_| !vsel_hot).map(|h| h.1));
    match vgrab {
        Some(Grab::Move) => ui.ctx().set_cursor_icon(if app.timeline.vdrag.is_some() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab }),
        Some(_) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal),
        None if vsel_hot && !end_hot => ui.ctx().set_cursor_icon(egui::CursorIcon::Text),
        None => {}
    }

    // Zoom with the wheel around the pointer; shift+wheel (or horizontal scroll) pans.
    if response.hovered() {
        let (scroll, zoom_mod) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
        if let Some(p) = response.hover_pos() {
            if scroll.x != 0.0 || (zoom_mod && scroll.y != 0.0) {
                let dx = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                app.timeline.start -= dx as f64 / px_per_s;
            } else if scroll.y != 0.0 {
                let anchor = t_of(p.x);
                let new_span = (span * (1.0 - scroll.y as f64 * 0.003)).clamp(0.5, total);
                app.timeline.start = anchor - (anchor - start) * new_span / span;
                app.timeline.span = if new_span >= total { None } else { Some(new_span) };
            }
        }
    }
    // Follow the playhead while playing.
    if app.playing && (app.time > start + span * 0.95 || app.time < start) {
        app.timeline.start = (app.time - span * 0.1).max(0.0);
    }

    if let Some((path, t)) = dropped {
        let audio = media::kind_of(&path) == Some(media::Kind::Audio);
        app.timeline.focus = if audio { Focus::Audio } else { Focus::Video };
        app.timeline.snapped = None;
        video::add_media(app, &ctx, &path, t);
    }

    // The section headers, last so they reflect this frame's focus.
    gutter_headers(app, &gp, gutter, ruler, &sections, drop_lane, &segments);
}

/// A video file's length for the drop ghost: probed once, in the background
/// (None until it's known).
fn drop_len(app: &mut VgApp, ctx: &egui::Context, path: &std::path::Path) -> Option<f64> {
    let (tx, rx) = app.timeline.drop_probe.get_or_insert_with(std::sync::mpsc::channel);
    while let Ok((p, len)) = rx.try_recv() {
        app.timeline.drop_lens.insert(p, len);
    }
    if let Some(len) = app.timeline.drop_lens.get(path) {
        return *len;
    }
    let (tx, p, ctx) = (tx.clone(), path.to_path_buf(), ctx.clone());
    app.timeline.drop_lens.insert(p.clone(), None);
    std::thread::spawn(move || {
        let len = vg_media::probe_info(&p).ok().and_then(|i| i.duration);
        let _ = tx.send((p, len));
        ctx.request_repaint();
    });
    None
}

/// The gutter: each section's key glyph and name (and a count, when there's
/// room). The section the keys act on is lit, with a bar on the left.
fn gutter_headers(app: &VgApp, gp: &egui::Painter, gutter: egui::Rect, ruler: egui::Rect, sections: &[(Lane, egui::Rect)], drop_lane: Option<Lane>, segments: &[(f64, f64)]) {
    gp.rect_filled(gutter, 0.0, egui::Color32::from_gray(0x0c));
    let narrow = gutter.width() < GUTTER;
    gp.line_segment([egui::pos2(gutter.left(), ruler.bottom()), egui::pos2(gutter.right(), ruler.bottom())], egui::Stroke::new(1.0_f32, theme::RULE));
    for (n, &(lane, r)) in sections.iter().enumerate() {
        let cell = egui::Rect::from_x_y_ranges(gutter.x_range(), r.y_range());
        let lit = drop_lane == Some(lane)
            || match lane {
                Lane::Audio => app.timeline.focus == Focus::Audio,
                Lane::Video => app.timeline.focus == Focus::Video,
                _ => false,
            };
        if lit {
            gp.rect_filled(cell, 0.0, egui::Color32::from_gray(0x1a));
            gp.rect_filled(egui::Rect::from_min_size(cell.min, egui::vec2(2.0, cell.height())), 0.0, theme::WHITE);
        } else if n % 2 == 1 {
            gp.rect_filled(cell, 0.0, egui::Color32::from_gray(0x10));
        }
        if n > 0 {
            gp.line_segment([cell.left_top(), cell.right_top()], egui::Stroke::new(1.0_f32, theme::RULE_SOFT));
        }
        let color = if lit { theme::WHITE } else { egui::Color32::from_gray(0x8a) };
        let icon_color = if lane == Lane::Poses { theme::MARK } else { color };
        if narrow {
            lane_icon(gp, egui::pos2(cell.center().x + 1.0, cell.center().y), lane, icon_color);
            continue;
        }
        let meta = match lane {
            Lane::Audio if segments.len() > 1 => format!("{} clips", segments.len()),
            Lane::Audio if app.audio.is_some() => "narration".into(),
            Lane::Transcript => app.parsed.timeline.as_ref().map(|t| format!("{} lines", t.spans.len())).unwrap_or_default(),
            Lane::Video if app.project.video.is_empty() => "empty".into(),
            Lane::Video => format!("{} clip{}", app.project.video.clips.len(), plural(app.project.video.clips.len())),
            Lane::Poses => app.parsed.timeline.as_ref().map(|t| format!("{} tags", t.events.len())).unwrap_or_default(),
            _ => String::new(),
        };
        let two_rows = cell.height() >= 32.0 && !meta.is_empty();
        let y = if two_rows { cell.top() + 12.0 } else { cell.center().y };
        lane_icon(gp, egui::pos2(cell.left() + 13.0, y), lane, icon_color);
        let g = tracked(gp, lane.name(), 8.5, color);
        gp.galley(egui::pos2(cell.left() + 24.0, y - g.size().y / 2.0), g, color);
        if two_rows {
            gp.text(egui::pos2(cell.left() + 24.0, y + 12.0), egui::Align2::LEFT_CENTER, meta, egui::FontId::proportional(9.0), egui::Color32::from_gray(if lit { 0x9a } else { 0x5c }));
        }
    }
    gp.line_segment([gutter.right_top(), gutter.right_bottom()], egui::Stroke::new(1.0_f32, theme::RULE));
}

/// A video clip's right-click menu.
fn clip_menu(app: &mut VgApp, response: &egui::Response, open: bool) {
    use egui::containers::SetOpenCommand;
    let cmd = if response.secondary_clicked() {
        Some(SetOpenCommand::Bool(open))
    } else if response.clicked() || response.drag_started() {
        Some(SetOpenCommand::Bool(false))
    } else {
        None
    };
    let Some(i) = app.timeline.menu_clip.filter(|&i| i < app.project.video.clips.len()) else { return };
    let c = app.project.video.clips[i].clone();
    let under = c.start < app.time && app.time < c.end();
    let mut act = None;
    egui::Popup::menu(response).open_memory(cmd).at_pointer_fixed().show(|ui| {
        ui.label(theme::caption(clip_name(&c)));
        if ui.add_enabled(under, egui::Button::new("Split at playhead")).clicked() {
            act = Some(VideoAct::Split);
        }
        if ui.button("Delete (leave a gap)").clicked() {
            act = Some(VideoAct::Delete(false));
        }
        if ui.button("Ripple delete").clicked() {
            act = Some(VideoAct::Delete(true));
        }
        if !c.still && ui.button(if c.muted { "Unmute" } else { "Mute" }).clicked() {
            act = Some(VideoAct::Mute);
        }
        if (c.fade_in > 0.0 || c.fade_out > 0.0) && ui.button("Remove fades").clicked() {
            act = Some(VideoAct::RemoveFades);
        }
        ui.separator();
        if ui.button("Show in the Media tab").clicked() {
            act = Some(VideoAct::Reveal);
        }
        if act.is_some() {
            ui.close();
        }
    });
    if let Some(a) = act {
        app.video.selected = Some(i);
        app.timeline.vsel = None;
        app.timeline.focus = Focus::Video;
        video_act(app, a);
    }
}

/// `1:23.5`, `83.5` or `1:02:03` as seconds.
fn parse_time(s: &str) -> Option<f64> {
    let mut t = 0.0;
    for part in s.trim().split(':') {
        t = t * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(t)
}

/// Set the video's length: `@duration`, or none when it matches the narration.
fn set_length(app: &mut VgApp, t: f64) {
    let narration = app.audio.as_ref().map(|a| a.duration);
    if narration.is_some_and(|n| (n - t).abs() < 0.005) {
        super::remove_directive(&mut app.cue_text, "duration");
        app.info("the video ends with the narration");
    } else {
        super::set_directive(&mut app.cue_text, "duration", &format!("{:.2}", t));
        app.info(format!("the video is {} long", format_time(t)));
    }
    app.reparse();
    if app.time > t {
        app.seek(t);
    }
}

fn tag_name(app: &VgApp, ev: &vg_core::Event) -> String {
    match ev.action {
        Action::Pose(p) => app.poses.poses[p].short_name().to_string(),
        Action::Hide => "hide".into(),
        Action::Show => "show".into(),
    }
}

fn transport(app: &mut VgApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    // Narrow panels drop the volume slider, and the focused section's actions
    // move to a second row when they don't fit beside the transport.
    let wide = ui.available_width() >= 1180.0;
    let mut fade_live = false;
    let mut second_row = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if theme::icon_button(ui, theme::Icon::ToStart, "Start · Home").clicked() {
            app.seek(0.0);
        }
        let (icon, tip) = if app.playing { (theme::Icon::Pause, "Pause · Space") } else { (theme::Icon::Play, "Play · Space") };
        if theme::icon_button(ui, icon, tip).clicked() {
            app.toggle_play();
        }
        ui.add_space(6.0);
        ui.label(egui::RichText::new(format_time(app.time)).size(16.0).color(theme::WHITE));
        ui.add_space(8.0);
        // The video's length: typed, dragged (the END handle) or the narration's.
        ui.label(theme::caption("Length"));
        let mut len = app.duration();
        let r = ui.add(egui::DragValue::new(&mut len).range(0.5..=36_000.0).speed(0.05).custom_formatter(|v, _| format_time(v)).custom_parser(parse_time));
        if r.drag_stopped() || (r.lost_focus() && r.changed()) || (r.changed() && !r.dragged() && !r.has_focus()) {
            set_length(app, len);
        } else if r.changed() && r.dragged() {
            app.timeline.end_drag = Some(len);
        }
        let fixed = app.parsed.sheet.as_ref().is_some_and(|s| s.duration.is_some());
        if fixed && app.audio.is_some() && theme::tool_button(ui, "Fit to narration", None, false).clicked() {
            super::remove_directive(&mut app.cue_text, "duration");
            app.reparse();
        }
        ui.add_space(10.0);
        let icon = if app.prefs.muted { theme::Icon::Muted } else { theme::Icon::Speaker };
        if theme::icon_button(ui, icon, if app.prefs.muted { "Unmute" } else { "Mute" }).clicked() {
            app.prefs.muted = !app.prefs.muted;
            if let Some(p) = &app.player {
                p.set_volume(app.prefs.out_volume());
            }
        }
        if wide {
            ui.spacing_mut().slider_width = 90.0;
            if ui.add(egui::Slider::new(&mut app.prefs.volume, 0.0..=1.0).show_value(false)).on_hover_text("Preview volume").changed()
                && let Some(p) = &app.player {
                    p.set_volume(app.prefs.out_volume());
                }
        }
        if theme::icon_button(ui, theme::Icon::Fit, "Fit the whole timeline").clicked() {
            app.timeline.fit_all();
        }
        ui.add_space(6.0);
        // The Audio tab's edit and the narration this tab plays.
        use studio::Narration;
        match studio::narration_state(app) {
            _ if app.studio.pending_sync => {
                theme::note(ui, "Preparing the edited audio…");
            }
            Narration::NeedsSync => {
                ui.label(egui::RichText::new("New cuts in the Audio tab").size(11.0).color(theme::TEXT2));
                if theme::tool_button(ui, "Apply to script", None, true).clicked() {
                    studio::sync_narration(app, &ctx);
                }
            }
            Narration::Unused => {
                theme::note(ui, "Playing the raw take");
                if theme::tool_button(ui, "Use the edited audio", None, false).clicked() {
                    studio::sync_narration(app, &ctx);
                }
            }
            Narration::Stale => {
                theme::note(ui, "Updating audio…");
            }
            _ => {}
        }
        // Snapping: off, to the captions (words, lines, poses) or to the video's clips.
        ui.add_space(6.0);
        ui.label(theme::caption("Snap"));
        let mut mode = app.prefs.snap_timeline.then_some(app.prefs.snap_to);
        if theme::segmented(ui, &mut mode, &[(None, "Off"), (Some(video::SnapTo::Captions), "Captions"), (Some(video::SnapTo::Video), "Video")]) {
            app.prefs.snap_timeline = mode.is_some();
            if let Some(m) = mode {
                app.prefs.snap_to = m;
            }
        }
        ui.add_space(10.0);
        if ui.available_width() >= section_width(app, ui) {
            fade_live = section_actions(app, ui, &ctx);
            key_hints(app, ui);
        } else {
            second_row = true;
        }
    });
    if second_row {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            fade_live = section_actions(app, ui, &ctx);
            key_hints(app, ui);
        });
    }
    // The fade fields' change becomes one undo step once they're let go.
    if !fade_live
        && let Some(before) = app.timeline.fade_before.take() {
            video::commit(app, before);
        }
}

/// Roughly how wide the focused section's actions are (to decide whether they
/// fit beside the transport).
fn section_width(app: &VgApp, ui: &egui::Ui) -> f32 {
    let text = |t: &str, font: egui::FontId| ui.painter().layout_no_wrap(t.to_string(), font, theme::WHITE).size().x;
    let button = |label: &str, key: Option<&str>| text(label, egui::FontId::proportional(12.5)) + 20.0 + key.map(|k| text(k, egui::FontId::monospace(9.5)) + 16.0).unwrap_or(0.0) + 6.0;
    let mut w = 60.0 + button("Split", Some("S"));
    if app.timeline.focus == Focus::Video {
        if app.timeline.vsel.is_some() {
            w += 90.0 + button("Delete", None) + button("Ripple delete", Some("Shift+Del"));
        } else if app.video.selected.is_some() {
            w += button("Delete", None) + button("Ripple delete", Some("Shift+Del")) + button("Mute", Some("M")) + 210.0;
        } else {
            w += 150.0;
        }
    } else if app.timeline.sel.is_some() {
        w += 90.0 + button("Delete", None) + button("Keep only", None);
    }
    if can_undo(app) {
        w += button("Undo", Some("Ctrl+Z"));
    }
    if video::can_redo(app) {
        w += button("Redo", Some("Ctrl+Y"));
    }
    w
}

/// The focused section's name and actions, and Undo / Redo. Returns whether a
/// fade field is being changed right now.
fn section_actions(app: &mut VgApp, ui: &mut egui::Ui, ctx: &egui::Context) -> bool {
    let on_video = app.timeline.focus == Focus::Video;
    ui.label(theme::caption(if on_video { "Video" } else { "Audio" }).color(theme::WHITE));
    let mut fade_live = false;
    if on_video {
        fade_live = video_actions(app, ui);
    } else if studio::narration_segments(app).is_some() {
        // Cutting the narration.
        use studio::Cut;
        let ready = !app.studio.pending_sync;
        if theme::tool_button_enabled(ui, "Split", Some("S"), false, ready).clicked() {
            cut(app, ctx, Cut::Split(app.time));
        }
        if let Some((a, b)) = app.timeline.sel {
            ui.label(egui::RichText::new(format!("{} selected", format_time(b - a))).size(11.0).color(theme::TEXT2));
            if ready && theme::danger_button(ui, "Delete").clicked() {
                cut(app, ctx, Cut::Delete(a, b));
            }
            if theme::tool_button_enabled(ui, "Keep only", None, false, ready).clicked() {
                cut(app, ctx, Cut::Keep(a, b));
            }
        }
    } else {
        theme::note(ui, "no narration to cut");
    }
    if can_undo(app) && theme::tool_button(ui, "Undo", Some("Ctrl+Z"), false).clicked() {
        undo(app);
    }
    if video::can_redo(app) && theme::tool_button(ui, "Redo", Some("Ctrl+Y"), false).clicked() {
        redo(app);
    }
    fade_live
}

/// Key hints for the focused section, right-aligned, when there's room.
fn key_hints(app: &VgApp, ui: &mut egui::Ui) {
    let hints: &[(&str, &str)] = if app.timeline.focus == Focus::Video {
        &[("select range", "Shift+drag"), ("fades", "drag ▼"), ("trim", "drag edge"), ("ripple", "Shift+Del"), ("lift", "Del"), ("mute", "M")]
    } else {
        &[("free", "Alt"), ("zoom", "scroll"), ("tag", "1–9"), ("frame", ", ."), ("edit points", "↑ ↓"), ("play", "J K L")]
    };
    let text = |t: &str, font: egui::FontId| ui.painter().layout_no_wrap(t.to_string(), font, theme::WHITE).size().x;
    let need: f32 = hints.iter().map(|(h, k)| text(h, egui::FontId::proportional(10.5)) + text(k, egui::FontId::monospace(9.5)) + 26.0).sum();
    if ui.available_width() < need + 20.0 {
        return;
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, (hint, key)) in hints.iter().enumerate() {
            if i > 0 {
                ui.add_space(6.0);
            }
            ui.label(egui::RichText::new(*hint).size(10.5).color(theme::MUTED));
            theme::key_chip(ui, key);
        }
    });
}

/// The Video section's transport actions. Returns whether a fade field is
/// being changed right now.
fn video_actions(app: &mut VgApp, ui: &mut egui::Ui) -> bool {
    let under = app.project.video.at(app.time).is_some_and(|i| {
        let c = &app.project.video.clips[i];
        app.time - c.start >= vg_project::video::MIN_CLIP && c.end() - app.time >= vg_project::video::MIN_CLIP
    });
    if theme::tool_button_enabled(ui, "Split", Some("S"), false, under).clicked() {
        video_act(app, VideoAct::Split);
    }
    let mut live = false;
    if let Some((a, b)) = app.timeline.vsel {
        ui.label(egui::RichText::new(format!("{} selected", format_time(b - a))).size(11.0).color(theme::TEXT2));
        if theme::danger_button(ui, "Delete").clicked() {
            video_act(app, VideoAct::Delete(false));
        }
        if theme::tool_button(ui, "Ripple delete", Some("Shift+Del"), false).clicked() {
            video_act(app, VideoAct::Delete(true));
        }
    } else if let Some(i) = app.video.selected.filter(|&i| i < app.project.video.clips.len()) {
        let c = app.project.video.clips[i].clone();
        if theme::danger_button(ui, "Delete").clicked() {
            video_act(app, VideoAct::Delete(false));
        }
        if theme::tool_button(ui, "Ripple delete", Some("Shift+Del"), false).clicked() {
            video_act(app, VideoAct::Delete(true));
        }
        if !c.still && theme::tool_button(ui, "Mute", Some("M"), c.muted).clicked() {
            video_act(app, VideoAct::Mute);
        }
        // Fades, in seconds: drag or type (one undo step when let go).
        ui.label(theme::caption("Fade"));
        let half = c.len() / 2.0;
        let (mut fi, mut fo) = (c.fade_in, c.fade_out);
        let r1 = ui.add(egui::DragValue::new(&mut fi).range(0.0..=half).speed(0.01).max_decimals(2).suffix(" s"));
        ui.label(egui::RichText::new("in ·").size(10.5).color(theme::MUTED));
        let r2 = ui.add(egui::DragValue::new(&mut fo).range(0.0..=half).speed(0.01).max_decimals(2).suffix(" s"));
        ui.label(egui::RichText::new("out").size(10.5).color(theme::MUTED));
        if r1.changed() || r2.changed() {
            if app.timeline.fade_before.is_none() {
                app.timeline.fade_before = Some(app.project.video.clone());
            }
            app.project.video.set_fades(i, fi, fo);
        }
        live = r1.dragged() || r2.dragged() || r1.has_focus() || r2.has_focus();
    } else if app.project.video.is_empty() {
        theme::note(ui, "add clips from the Media tab");
    } else {
        theme::note(ui, "click a clip to edit it");
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_block_snaps_by_its_nearer_edge() {
        let targets = [0.0, 10.0, 20.0];
        // The start is 0.25 from 10, the end 0.125 from 20: the end wins.
        assert_eq!(snap_span(9.75, 10.125, &targets, 0.5), (9.875, Some(20.0)));
        // Only the start is near.
        assert_eq!(snap_span(9.8, 3.0, &targets, 0.5), (10.0, Some(10.0)));
        // Nothing near: unchanged.
        assert_eq!(snap_span(4.0, 3.0, &targets, 0.5), (4.0, None));
    }

    #[test]
    fn presses_on_a_clip_grab_handles_edges_then_the_body() {
        // A clip from x 100 to 200, top at y 50, fades ending at 120 and 190.
        let g = |x, y| grab_at(x, y, 100.0, 200.0, 50.0, 120.0, 190.0);
        assert_eq!(g(121.0, 52.0), Grab::FadeIn);
        assert_eq!(g(188.0, 55.0), Grab::FadeOut);
        // Lower down, the same spot moves the clip.
        assert_eq!(g(121.0, 70.0), Grab::Move);
        assert_eq!(g(102.0, 70.0), Grab::Start);
        assert_eq!(g(199.0, 70.0), Grab::End);
        // A narrow clip still has a middle.
        assert_eq!(grab_at(105.0, 70.0, 100.0, 110.0, 50.0, 104.0, 106.0), Grab::Move);
    }
}
