//! The left sidebar's tabs (Poses, Media) and the Media tab: the images, video
//! and audio imported into the project, as thumbnail cards in one section per
//! kind, with an info panel for the selected card docked under them. Cards
//! are added to the timeline with a double-click, the panel's button, or by
//! dragging them onto it. The list is saved in the project (`media`); removing
//! a file from it leaves the file on disk.

use super::util::{AUDIO_EXTS, IMAGE_EXTS, THUMB, VIDEO_EXTS, file_name, thumbnail};
use super::{VgApp, theme};
use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::SystemTime;
use vg_core::paths::canon;

/// Bars in an audio file's little waveform.
const BARS: usize = 40;
/// Narrowest a card gets before the grid drops a column.
const CARD_MIN_W: f32 = 104.0;
const CARD_GAP: f32 = 6.0;

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sidebar {
    #[default]
    Poses,
    Media,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Image,
    Video,
    Audio,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Image, Kind::Video, Kind::Audio];

    fn title(self) -> &'static str {
        match self {
            Kind::Image => "Images",
            Kind::Video => "Video",
            Kind::Audio => "Audio",
        }
    }

    /// One of them, for the info panel's title.
    fn one(self) -> &'static str {
        match self {
            Kind::Image => "Image",
            Kind::Video => "Video",
            Kind::Audio => "Audio",
        }
    }

    fn exts(self) -> &'static [&'static str] {
        match self {
            Kind::Image => IMAGE_EXTS,
            Kind::Video => VIDEO_EXTS,
            Kind::Audio => AUDIO_EXTS,
        }
    }
}

pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    Kind::ALL.into_iter().find(|k| k.exts().contains(&ext.as_str()))
}

/// What's known about a file, filled in off the UI thread.
#[derive(Default)]
struct Info {
    loading: bool,
    missing: bool,
    bytes: u64,
    modified: Option<SystemTime>,
    size: Option<(u32, u32)>,
    duration: Option<f64>,
    /// ffprobe's streams (video and audio files).
    probe: Option<vg_media::MediaInfo>,
    thumb: Option<egui::TextureHandle>,
    peaks: Vec<f32>,
    error: Option<String>,
}

/// A loader's result: the file's details, and its picture to upload.
struct Loaded {
    path: PathBuf,
    info: Info,
    image: Option<egui::ColorImage>,
}

pub struct Media {
    /// Keyed by the resolved path.
    info: HashMap<PathBuf, Info>,
    tx: mpsc::Sender<Loaded>,
    rx: mpsc::Receiver<Loaded>,
    /// The card whose info panel is open (resolved path).
    selected: Option<PathBuf>,
    /// The info panel's height last frame, to leave room for it under the cards.
    panel_h: f32,
    /// Frames left to keep the selected card in view (it was just picked, and
    /// the panel opening under it may cover it).
    reveal: u8,
    /// Where the selected card sits in the scrolled list (top, bottom), and
    /// how far the list was scrolled, last frame.
    sel_span: Option<(f32, f32)>,
    grid_offset: f32,
}

impl Media {
    /// Block until every file being read has been read (for offscreen captures).
    pub fn wait(&mut self) {
        let mut waiting = self.info.values().filter(|i| i.loading).count();
        let mut got = vec![];
        while waiting > 0 {
            match self.rx.recv_timeout(std::time::Duration::from_secs(20)) {
                Ok(l) => {
                    got.push(l);
                    waiting -= 1;
                }
                Err(_) => break,
            }
        }
        for l in got {
            let _ = self.tx.send(l);
        }
    }
}

impl Default for Media {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Media { info: HashMap::new(), tx, rx, selected: None, panel_h: 260.0, reveal: 0, sel_span: None, grid_offset: 0.0 }
    }
}

fn load(path: PathBuf, kind: Kind) -> Loaded {
    let meta = std::fs::metadata(&path);
    let mut info = Info { missing: meta.is_err(), modified: meta.as_ref().ok().and_then(|m| m.modified().ok()), bytes: meta.map(|m| m.len()).unwrap_or(0), ..Default::default() };
    let mut pic = None;
    if info.missing {
        return Loaded { path, info, image: pic };
    }
    let r: anyhow::Result<()> = (|| {
        match kind {
            Kind::Image => {
                let img = image::open(&path)?;
                info.size = Some((img.width(), img.height()));
                pic = Some(thumbnail(&img));
            }
            Kind::Video => {
                let probe = vg_media::probe_info(&path)?;
                let v = probe.video.clone();
                info.duration = probe.duration;
                info.probe = Some(probe);
                let Some(v) = v else { anyhow::bail!("it has no picture") };
                info.size = Some((v.width, v.height));
                // A frame a little way in: the very first is often black.
                let t = info.duration.map(|d| (d * 0.1).min(2.0)).unwrap_or(0.0);
                let png = vg_media::video_frame_png(&path, t, THUMB)?;
                pic = Some(thumbnail(&image::load_from_memory(&png)?));
            }
            Kind::Audio => {
                // The details are nice to have; the length is needed.
                let probe = vg_media::probe_info(&path).ok();
                let d = match probe.as_ref().and_then(|i| i.duration) {
                    Some(d) => d,
                    None => vg_media::probe_duration(&path)?,
                };
                info.probe = probe;
                info.duration = Some(d);
                let rate = ((BARS as f64 * 4.0 / d.max(0.1)).ceil() as u32).clamp(1, 200);
                let peaks = vg_media::waveform_peaks(&path, rate)?;
                let n = peaks.len().max(1);
                info.peaks = (0..BARS).map(|b| peaks[b * n / BARS..((b + 1) * n / BARS).max(b * n / BARS + 1).min(n)].iter().fold(0.0_f32, |a, &p| a.max(p))).collect();
                // Scaled to the loudest bar, so quiet recordings still show a shape.
                let top = info.peaks.iter().fold(0.0_f32, |a, &p| a.max(p)).max(1e-4);
                info.peaks.iter_mut().for_each(|p| *p /= top);
            }
        }
        Ok(())
    })();
    info.error = r.err().map(|e| format!("{e:#}"));
    Loaded { path, info, image: pic }
}

/// Files chosen in an Import dialog.
pub(super) fn imported(app: &mut VgApp, kind: Kind, files: &[PathBuf]) {
    let n = add(app, files);
    app.prefs.sidebar = Sidebar::Media;
    app.info(format!("added {n} to {} · Ctrl+S saves the project", kind.title()));
}

/// Add files to the project's media (skipping ones it has and ones that aren't
/// images, video or audio). Returns how many were added.
pub fn add(app: &mut VgApp, paths: &[PathBuf]) -> usize {
    let base = app.base();
    let have: Vec<PathBuf> = app.project.media.iter().map(|m| canon(&vg_project::resolve(&base, m))).collect();
    let mut added = 0;
    for p in paths {
        if kind_of(p).is_none() || have.contains(&canon(p)) {
            continue;
        }
        app.project.media.push(super::relative_path(p, &base));
        added += 1;
    }
    added
}

enum Act {
    Import(Kind),
    /// Click on a card: select it, or deselect it if it's the selected one.
    Toggle(PathBuf),
    Deselect,
    Add(PathBuf),
    Remove(PathBuf),
    Narration(PathBuf),
    EditAudio(PathBuf),
    Background(PathBuf),
    Copy(PathBuf),
}

/// The sidebar: tabs, then the chosen one.
pub fn sidebar(app: &mut VgApp, ui: &mut egui::Ui) {
    take_drops(app, ui.ctx());
    let tabs = [(Sidebar::Poses, "Poses", app.poses.poses.len().to_string()), (Sidebar::Media, "Media", app.project.media.len().to_string())];
    theme::tabs(ui, &mut app.prefs.sidebar, &tabs);
    match app.prefs.sidebar {
        Sidebar::Poses => super::palette::show(app, ui),
        Sidebar::Media => show(app, ui),
    }
}

/// Files dropped on the window go into the media list (and show it).
fn take_drops(app: &mut VgApp, ctx: &egui::Context) {
    let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
    if dropped.is_empty() {
        return;
    }
    let n = add(app, &dropped);
    app.prefs.sidebar = Sidebar::Media;
    let skipped = dropped.len() - n;
    match (n, skipped) {
        (0, _) => app.error("nothing added: those are already in the project, or aren't images, video or audio"),
        (n, 0) => app.info(format!("added {n} to Media · Ctrl+S saves the project")),
        (n, s) => app.info(format!("added {n} to Media ({s} skipped) · Ctrl+S saves the project")),
    }
}

/// A file's thumbnail (image, video frame) from the Media cache, starting to
/// load it if needed: (texture, its size). For timeline clips.
pub(super) fn thumb(app: &mut VgApp, ctx: &egui::Context, path: &Path) -> Option<(egui::TextureId, egui::Vec2)> {
    receive(app, ctx);
    if !app.media.info.contains_key(path) {
        let kind = kind_of(path)?;
        spawn_load(app, ctx, path.to_path_buf(), kind);
        return None;
    }
    app.media.info.get(path)?.thumb.as_ref().map(|t| (t.id(), t.size_vec2()))
}

fn spawn_load(app: &mut VgApp, ctx: &egui::Context, p: PathBuf, kind: Kind) {
    app.media.info.insert(p.clone(), Info { loading: true, ..Default::default() });
    let (tx, ctx) = (app.media.tx.clone(), ctx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(load(p, kind));
        ctx.request_repaint();
    });
}

/// Results from the loaders.
fn receive(app: &mut VgApp, ctx: &egui::Context) {
    while let Ok(Loaded { path, mut info, image }) = app.media.rx.try_recv() {
        info.thumb = image.map(|img| ctx.load_texture(format!("media-{}", path.display()), img, egui::TextureOptions::LINEAR));
        app.media.info.insert(path, info);
    }
}

/// How a file is used in the project: for the cards' markers and the panel.
#[derive(Default, Clone, Copy)]
struct Uses {
    /// Clips on the video track.
    clips: usize,
    /// Tracks in the narration's edit (the Audio section).
    tracks: usize,
    narration: bool,
    background: bool,
}

impl Uses {
    fn on_timeline(&self) -> bool {
        self.clips > 0 || self.tracks > 0 || self.narration
    }

    /// Amber: the current narration or preview background.
    fn current(&self) -> bool {
        self.narration || self.background
    }
}

/// Every file the project uses, keyed by its canonical path.
fn uses(app: &VgApp) -> HashMap<PathBuf, Uses> {
    let mut map: HashMap<PathBuf, Uses> = HashMap::new();
    let base = app.base();
    for c in &app.project.video.clips {
        map.entry(canon(&vg_project::resolve(&base, &c.source))).or_default().clips += 1;
    }
    let mix_base = app.studio.base();
    for t in &app.studio.mix.tracks {
        map.entry(canon(&vg_project::resolve(&mix_base, &t.source))).or_default().tracks += 1;
    }
    if let Some(n) = &app.parsed.audio_path {
        map.entry(canon(n)).or_default().narration = true;
    }
    if let Some(bg) = app.prefs.preview_bg_image.as_deref().filter(|_| app.prefs.preview_bg == super::preview::Background::Image) {
        map.entry(canon(bg)).or_default().background = true;
    }
    map
}

fn show(app: &mut VgApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    receive(app, &ctx);
    let base = app.base();
    // (as saved, resolved, kind)
    let items: Vec<(PathBuf, PathBuf, Kind)> = app
        .project
        .media
        .iter()
        .filter_map(|m| {
            let abs = vg_project::resolve(&base, m);
            Some((m.clone(), abs.clone(), kind_of(&abs)?))
        })
        .collect();
    for (_, abs, kind) in &items {
        if !app.media.info.contains_key(abs) {
            spawn_load(app, &ctx, abs.clone(), *kind);
        }
    }
    // The selection goes when its file leaves the list.
    let selected = app.media.selected.as_ref().and_then(|s| items.iter().find(|x| &x.1 == s)).cloned();
    if selected.is_none() {
        app.media.selected = None;
    }
    let uses = uses(app);
    let use_of = |abs: &Path| uses.get(&canon(abs)).copied().unwrap_or_default();

    let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let mut act = None;
    theme::weak(ui, "Import or drop files on the window; they stay where they are. Click a card for its details, double-click to add it at the playhead, or drag it onto the timeline.");
    ui.add_space(2.0);
    if selected.is_some() && ui.rect_contains_pointer(ui.max_rect()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        act = Some(Act::Deselect);
    }

    // The info panel is docked at the bottom; the cards scroll in the rest.
    const GAP: f32 = 8.0;
    let avail = ui.available_height();
    // The cards keep at least a row's worth of room.
    let panel_max = (avail - 170.0).max(120.0);
    let panel_h = if selected.is_some() { app.media.panel_h.min(panel_max) + GAP } else { 0.0 };
    let dragging = egui::DragAndDrop::payload::<super::video::MediaDrop>(&ctx);
    let frame = egui::Frame::new().stroke(egui::Stroke::new(1.0_f32, if hovering { theme::FOCUS } else { egui::Color32::TRANSPARENT }));
    let grid_h = (avail - panel_h - 2.0).max(60.0);
    let mut scroll = egui::ScrollArea::vertical().id_salt("media-list").max_height(grid_h).auto_shrink([false, false]);
    // Keep a just-picked card in view as the panel opens under it.
    if let Some((top, bottom)) = app.media.sel_span.filter(|_| app.media.reveal > 0) {
        let o = app.media.grid_offset;
        if top < o {
            scroll = scroll.vertical_scroll_offset((top - 4.0).max(0.0));
        } else if bottom > o + grid_h {
            scroll = scroll.vertical_scroll_offset(bottom - grid_h + 4.0);
        }
    }
    let mut sel_span = None;
    frame.show(ui, |ui| {
        let out = scroll.show(ui, |ui| {
            let content_top = ui.min_rect().top();
            // Room on the right for the scroll bar, so it never covers a card.
            ui.set_max_width(ui.available_width() - 10.0);
            for kind in Kind::ALL {
                let these: Vec<_> = items.iter().filter(|x| x.2 == kind).collect();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(theme::caption(kind.title()).color(theme::WHITE));
                    theme::note(ui, these.len().to_string());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::tool_button(ui, "Import", None, false).clicked() {
                            act = Some(Act::Import(kind));
                        }
                    });
                });
                rule(ui, theme::RULE);
                if these.is_empty() {
                    ui.add_space(2.0);
                    theme::weak(ui, match kind {
                        Kind::Image => "No images yet: backgrounds, screenshots, stills.",
                        Kind::Video => "No video yet: screen captures and footage.",
                        Kind::Audio => "No audio yet: music, effects, other takes.",
                    });
                    continue;
                }
                ui.add_space(2.0);
                // Cards reflow to the sidebar's width.
                let w = ui.available_width();
                // Two columns even in a narrow sidebar, while they still fit.
                let cols = ((w + CARD_GAP) / (CARD_MIN_W + CARD_GAP)).floor().max(if w >= 150.0 { 2.0 } else { 1.0 }) as usize;
                let cell_w = ((w - CARD_GAP * (cols as f32 - 1.0)) / cols as f32).floor();
                let thumb_h = ((cell_w - 10.0) * 0.62).round();
                let cell = egui::vec2(cell_w, thumb_h + 10.0 + 30.0);
                for row in these.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = CARD_GAP;
                        for (_, abs, kind) in row {
                            let c = Card {
                                abs,
                                kind: *kind,
                                info: app.media.info.get(abs),
                                uses: use_of(abs),
                                selected: app.media.selected.as_ref() == Some(abs),
                                dragged: dragging.as_ref().is_some_and(|d| &d.0 == abs),
                            };
                            let resp = card(ui, &c, cell, thumb_h);
                            if c.selected {
                                sel_span = Some((resp.rect.top() - content_top, resp.rect.bottom() - content_top));
                            }
                            resp.dnd_set_drag_payload(super::video::MediaDrop(abs.clone()));
                            if resp.double_clicked() {
                                act = Some(Act::Add(abs.clone()));
                            } else if resp.clicked() {
                                act = Some(Act::Toggle(abs.clone()));
                            }
                        }
                    });
                    ui.add_space(CARD_GAP - ui.spacing().item_spacing.y);
                }
            }
        });
        app.media.grid_offset = out.state.offset.y;
    });
    app.media.sel_span = sel_span;

    if let Some((rel, abs, kind)) = &selected {
        let r = egui::ScrollArea::vertical().id_salt("media-info").max_height(panel_max).auto_shrink([false, true]).show(ui, |ui| panel(app, ui, rel, abs, *kind, use_of(abs)));
        if let Some(a) = r.inner {
            act = Some(a);
        }
        // Its full height (the scroll area is held to the room left this frame).
        let h = r.content_size.y.min(panel_max);
        if (h - app.media.panel_h).abs() > 0.5 {
            app.media.panel_h = h;
            ctx.request_discard("media info panel height");
        }
    }

    app.media.reveal = app.media.reveal.saturating_sub(1);
    if let Some(d) = &dragging {
        drag_label(app, &ctx, &d.0);
    }

    let Some(act) = act else { return };
    match act {
        Act::Import(kind) => {
            let d = rfd::FileDialog::new().set_title(format!("Import {}", kind.title().to_lowercase())).add_filter(kind.title(), kind.exts()).set_directory(app.base());
            super::dialog::open(app, super::dialog::Want::MediaImport(kind), d, super::dialog::How::Files);
        }
        Act::Toggle(p) => {
            app.media.selected = if app.media.selected.as_ref() == Some(&p) { None } else { Some(p) };
            app.media.reveal = 4;
        }
        Act::Deselect => app.media.selected = None,
        Act::Add(p) => {
            app.media.selected = Some(p.clone());
            let t = app.time;
            super::video::add_media(app, &ctx, &p, t);
        }
        Act::Remove(rel) => {
            let abs = vg_project::resolve(&base, &rel);
            app.project.media.retain(|m| *m != rel);
            if app.media.selected.as_ref() == Some(&abs) {
                app.media.selected = None;
            }
            app.media.info.remove(&abs);
            app.info(format!("removed {} from the project (the file is still there)", rel.display()));
        }
        Act::Narration(p) => {
            app.set_narration(&p);
            app.use_saved_words(&p);
            app.info(format!("narration: {} (Ctrl+S saves the script)", file_name(&p)));
        }
        Act::EditAudio(p) => {
            super::recordings::edit(app, &p);
            if app.studio.loaded_for.is_some() {
                app.prefs.view = super::View::Audio;
            }
        }
        Act::Background(p) => {
            app.prefs.preview_bg_image = Some(p);
            app.prefs.preview_bg = super::preview::Background::Image;
        }
        Act::Copy(p) => {
            ctx.copy_text(p.display().to_string());
            app.info(format!("copied {}", p.display()));
        }
    }
}

struct Card<'a> {
    abs: &'a Path,
    kind: Kind,
    info: Option<&'a Info>,
    uses: Uses,
    selected: bool,
    /// Being dragged to the timeline right now.
    dragged: bool,
}

/// One thumbnail card: the picture (a frame of video, the audio's waveform)
/// with its badges, then the name and a line of meta.
fn card(ui: &mut egui::Ui, c: &Card, size: egui::Vec2, thumb_h: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let p = ui.painter();
    let missing = c.info.is_some_and(|i| i.missing);
    let hovered = resp.hovered() && !c.dragged;
    let lit = hovered || c.selected;
    if lit {
        p.rect_filled(rect, 1.0, theme::HOVER);
    }
    if c.dragged {
        theme::dashed_rect(p, rect.shrink(0.5), egui::Stroke::new(1.0_f32, theme::FOCUS));
    } else {
        let edge = if c.selected {
            theme::WHITE
        } else if missing {
            theme::DANGER
        } else if c.uses.current() {
            theme::MARK
        } else if hovered {
            theme::FOCUS
        } else {
            theme::RULE
        };
        p.rect_stroke(rect, 1.0, egui::Stroke::new(if c.selected { 1.5_f32 } else { 1.0 }, edge), egui::StrokeKind::Inside);
    }
    if c.selected {
        theme::glow_rect(p, rect, 0.12);
    }

    let tr = egui::Rect::from_min_size(rect.min + egui::vec2(5.0, 5.0), egui::vec2(rect.width() - 10.0, thumb_h));
    p.rect_filled(tr, 0.0, egui::Color32::from_gray(0x0a));
    match c.info {
        Some(Info { thumb: Some(tex), .. }) => {
            let tint = if lit { egui::Color32::WHITE } else { egui::Color32::from_gray(215) };
            theme::image_fit(p, tex.id(), tex.size_vec2(), tr, tint);
        }
        Some(i) if !i.peaks.is_empty() => {
            let inner = tr.shrink2(egui::vec2(4.0, 6.0));
            let w = inner.width() / i.peaks.len() as f32;
            let col = if c.uses.narration { theme::MARK } else if lit { theme::WHITE } else { theme::TEXT2 };
            p.hline(inner.x_range(), inner.center().y, egui::Stroke::new(1.0_f32, theme::RULE_SOFT));
            for (b, &pk) in i.peaks.iter().enumerate() {
                let h = (pk.clamp(0.0, 1.0) * inner.height()).max(1.0);
                let x = inner.left() + (b as f32 + 0.5) * w;
                p.line_segment([egui::pos2(x, inner.center().y - h / 2.0), egui::pos2(x, inner.center().y + h / 2.0)], egui::Stroke::new((w - 1.0).max(1.0), col));
            }
        }
        _ => {
            let (text, col) = if missing {
                ("MISSING", theme::DANGER)
            } else if c.info.is_some_and(|i| i.loading) {
                ("reading…", theme::MUTED)
            } else if c.info.is_some_and(|i| i.error.is_some()) {
                ("can't read it", theme::DANGER)
            } else {
                (c.kind.one(), theme::MUTED)
            };
            p.text(tr.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(10.5), col);
        }
    }
    if missing {
        // A red cross through the empty frame, so it reads at a glance.
        let s = egui::Stroke::new(1.0_f32, theme::DANGER.gamma_multiply(0.35));
        p.line_segment([tr.left_top(), tr.right_bottom()], s);
        p.line_segment([tr.right_top(), tr.left_bottom()], s);
    }

    // Badges: what's playable, how long, and how it's used.
    let has_thumb = c.info.is_some_and(|i| i.thumb.is_some());
    if c.kind == Kind::Video && has_thumb {
        let ctr = tr.left_bottom() + egui::vec2(11.0, -11.0);
        p.circle_filled(ctr, 7.5, egui::Color32::from_black_alpha(190));
        p.circle_stroke(ctr, 7.5, egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(90)));
        p.add(egui::Shape::convex_polygon(vec![ctr + egui::vec2(-2.5, -4.0), ctr + egui::vec2(4.0, 0.0), ctr + egui::vec2(-2.5, 4.0)], theme::WHITE, egui::Stroke::NONE));
    }
    if let Some(d) = c.info.and_then(|i| i.duration).filter(|_| c.kind != Kind::Image) {
        let g = p.layout_no_wrap(clock(d), egui::FontId::monospace(9.5), theme::WHITE);
        let br = egui::Rect::from_min_size(tr.right_bottom() - g.size() - egui::vec2(9.0, 7.0), g.size() + egui::vec2(6.0, 3.0));
        p.rect_filled(br, 1.0, egui::Color32::from_black_alpha(200));
        p.galley(br.min + egui::vec2(3.0, 1.5), g, theme::WHITE);
    }
    let mut chips: Vec<(&str, egui::Color32)> = vec![];
    if c.uses.narration {
        chips.push(("Narration", theme::MARK));
    }
    if c.uses.background {
        chips.push(("Background", theme::MARK));
    }
    if c.uses.clips > 0 || c.uses.tracks > 0 {
        chips.push(("On timeline", theme::WHITE));
    }
    let mut y = tr.top() + 4.0;
    for (text, col) in chips {
        y = chip(p, egui::pos2(tr.left() + 4.0, y), text, col).bottom() + 3.0;
    }
    if c.uses.current() {
        theme::marked_corner(p, rect);
    }

    // Name and meta, cut to the card's width.
    let text_w = rect.width() - 12.0;
    let name_col = if missing {
        theme::DANGER
    } else if c.uses.current() {
        theme::MARK
    } else if lit {
        theme::WHITE
    } else {
        theme::TEXT2
    };
    let g = p.layout_job(one_line(&file_name(c.abs), egui::FontId::proportional(11.5), name_col, text_w));
    p.galley(egui::pos2(rect.left() + 6.0, tr.bottom() + 4.0), g, name_col);
    let meta = if missing {
        "not found".to_string()
    } else {
        match c.info {
            Some(i) if i.loading => "reading…".into(),
            Some(i) => {
                let mut m = vec![];
                if let Some((w, h)) = i.size {
                    m.push(format!("{w}×{h}"));
                }
                if i.bytes > 0 {
                    m.push(bytes(i.bytes));
                }
                m.join(" · ")
            }
            None => String::new(),
        }
    };
    let g = p.layout_job(one_line(&meta, egui::FontId::proportional(10.0), if missing { theme::DANGER } else { theme::MUTED }, text_w));
    p.galley(egui::pos2(rect.left() + 6.0, tr.bottom() + 19.0), g, theme::MUTED);
    resp
}

/// A small uppercase label on a card's picture; returns where it went.
fn chip(p: &egui::Painter, at: egui::Pos2, text: &str, col: egui::Color32) -> egui::Rect {
    let mut job = egui::text::LayoutJob::default();
    job.append(&text.to_uppercase(), 0.0, egui::TextFormat { font_id: egui::FontId::proportional(8.5), color: col, extra_letter_spacing: 0.7, ..Default::default() });
    let g = p.layout_job(job);
    let r = egui::Rect::from_min_size(at, g.size() + egui::vec2(8.0, 3.0));
    p.rect_filled(r, 1.0, egui::Color32::from_black_alpha(210));
    p.rect_stroke(r, 1.0, egui::Stroke::new(1.0_f32, col.gamma_multiply(0.7)), egui::StrokeKind::Inside);
    p.galley(r.min + egui::vec2(4.0, 1.5), g, col);
    r
}

/// Text laid out on one line, ending in "…" if it doesn't fit.
fn one_line(text: &str, font: egui::FontId, color: egui::Color32, width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat { font_id: font, color, ..Default::default() });
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job
}

/// While a card is dragged: its name (and picture) by the pointer.
fn drag_label(app: &VgApp, ctx: &egui::Context, path: &Path) {
    let Some(pos) = ctx.pointer_latest_pos() else { return };
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("media-drag")));
    let g = p.layout_no_wrap(file_name(path), egui::FontId::proportional(12.0), theme::WHITE);
    let hint = p.layout_no_wrap("drop on the timeline".into(), egui::FontId::proportional(10.0), theme::MUTED);
    let tex = app.media.info.get(path).and_then(|i| i.thumb.as_ref()).map(|t| (t.id(), t.size_vec2()));
    let pic_w = if tex.is_some() { 44.0 } else { 0.0 };
    let size = egui::vec2(pic_w + g.size().x.max(hint.size().x) + 16.0, 36.0);
    let r = egui::Rect::from_min_size(pos + egui::vec2(14.0, 12.0), size);
    p.rect_filled(r, 1.0, theme::BLACK);
    p.rect_stroke(r, 1.0, egui::Stroke::new(1.0_f32, theme::FOCUS), egui::StrokeKind::Inside);
    theme::glow_rect(&p, r, 0.10);
    if let Some((id, s)) = tex {
        theme::image_fit(&p, id, s, egui::Rect::from_min_size(r.min + egui::vec2(6.0, 5.0), egui::vec2(38.0, 26.0)), egui::Color32::WHITE);
    }
    let x = r.left() + 8.0 + pic_w;
    p.galley(egui::pos2(x, r.top() + 3.0), g, theme::WHITE);
    p.galley(egui::pos2(x, r.top() + 20.0), hint, theme::MUTED);
}

/// The info panel for the selected card: what the file is, where it's used,
/// and what to do with it.
fn panel(app: &VgApp, ui: &mut egui::Ui, rel: &Path, abs: &Path, kind: Kind, uses: Uses) -> Option<Act> {
    let mut act = None;
    let info = app.media.info.get(abs);
    let missing = info.is_some_and(|i| i.missing);
    rule(ui, theme::RULE);
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let ext = abs.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
        theme::panel_title(ui, kind.one(), &ext);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if close_button(ui).clicked() {
                act = Some(Act::Deselect);
            }
        });
    });
    let name_col = if missing { theme::DANGER } else if uses.current() { theme::MARK } else { theme::WHITE };
    ui.add(egui::Label::new(egui::RichText::new(file_name(abs)).size(13.0).color(name_col)).wrap());
    let value = |ui: &mut egui::Ui, text: String| {
        ui.add(egui::Label::new(egui::RichText::new(text).size(11.5).color(theme::TEXT2)).wrap());
    };
    let folder = abs.parent().map(folder).unwrap_or_default();

    if missing {
        ui.add(egui::Label::new(egui::RichText::new("Not found. It was moved, renamed or deleted; the project still lists it.").size(11.5).color(theme::DANGER)).wrap());
        egui::Grid::new("media-info-missing").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
            ui.label(theme::caption("Was in"));
            value(ui, folder);
            ui.end_row();
        });
        ui.add_space(4.0);
        if theme::tool_button(ui, "Remove from project", None, false).clicked() {
            act = Some(Act::Remove(rel.to_path_buf()));
        }
        ui.add_space(4.0);
        return act;
    }

    ui.add_space(4.0);
    if primary_button(ui, &format!("Add to timeline at {}", vg_core::format_time(app.time))).clicked() {
        act = Some(Act::Add(abs.to_path_buf()));
    }
    theme::weak(ui, match kind {
        Kind::Audio => "As an Audio track under the narration · or double-click the card",
        _ => "On the video track at the playhead · or double-click the card",
    });
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        match kind {
            Kind::Audio => {
                if theme::tool_button_enabled(ui, if uses.narration { "The narration" } else { "Use as narration" }, None, uses.narration, !uses.narration).clicked() {
                    act = Some(Act::Narration(abs.to_path_buf()));
                }
                if theme::tool_button(ui, "Edit", None, false).clicked() {
                    act = Some(Act::EditAudio(abs.to_path_buf()));
                }
            }
            Kind::Image => {
                if theme::tool_button_enabled(ui, if uses.background { "The background" } else { "Use as background" }, None, uses.background, !uses.background).clicked() {
                    act = Some(Act::Background(abs.to_path_buf()));
                }
            }
            Kind::Video => {}
        }
        if theme::tool_button(ui, "Copy path", None, false).clicked() {
            act = Some(Act::Copy(abs.to_path_buf()));
        }
        if theme::tool_button(ui, "Remove from project", None, false).clicked() {
            act = Some(Act::Remove(rel.to_path_buf()));
        }
    });
    ui.add_space(6.0);
    rule(ui, theme::RULE_SOFT);
    ui.add_space(2.0);
    egui::Grid::new("media-info").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
        let row = |ui: &mut egui::Ui, label: &str, text: String| {
            ui.label(theme::caption(label));
            value(ui, text);
            ui.end_row();
        };
        row(ui, "Folder", folder);
        let Some(i) = info else { return };
        if i.loading {
            row(ui, "Details", "reading…".into());
            return;
        }
        let probe = i.probe.as_ref();
        let rate = probe.and_then(|p| p.bit_rate).map(|b| format!(" · {}", bit_rate(b))).unwrap_or_default();
        row(ui, "File size", format!("{}{rate}", bytes(i.bytes)));
        if let Some(m) = i.modified {
            row(ui, "Modified", super::util::ago(Some(m)));
        }
        if let Some(d) = i.duration {
            row(ui, "Length", vg_core::format_time(d));
        }
        match kind {
            Kind::Image => {
                if let Some((w, h)) = i.size {
                    row(ui, "Picture", format!("{w}×{h}"));
                }
            }
            Kind::Video => {
                if let Some(v) = probe.and_then(|p| p.video.as_ref()) {
                    let mut pic = vec![format!("{}×{}", v.width, v.height)];
                    if let Some(f) = v.fps {
                        pic.push(format!("{} fps", fps(f)));
                    }
                    pic.push(codec(&v.codec));
                    row(ui, "Picture", pic.join(" · "));
                    if v.rotation.rem_euclid(360) != 0 {
                        let turned = v.rotation.rem_euclid(180) == 90;
                        let (w, h) = if turned { (v.height, v.width) } else { (v.width, v.height) };
                        row(ui, "Rotation", format!("{}° · shows as {w}×{h}", v.rotation.rem_euclid(360)));
                    }
                }
                let sound = match probe.and_then(|p| p.audio.as_ref()) {
                    Some(a) => sound(a),
                    None if probe.is_some() => "no sound".into(),
                    None => "—".into(),
                };
                row(ui, "Sound", sound);
            }
            Kind::Audio => {
                if let Some(a) = probe.and_then(|p| p.audio.as_ref()) {
                    row(ui, "Sound", sound(a));
                }
            }
        }
        if let Some(e) = &i.error {
            ui.label(theme::caption("Problem"));
            ui.add(egui::Label::new(egui::RichText::new(e).size(11.0).color(theme::DANGER)).wrap());
            ui.end_row();
        }
        let mut used = vec![];
        if uses.clips > 0 {
            used.push(if uses.clips == 1 { "on the video track".to_string() } else { format!("on the video track ×{}", uses.clips) });
        }
        if uses.tracks > 0 {
            used.push(if uses.tracks == 1 { "Audio track".to_string() } else { format!("Audio track ×{}", uses.tracks) });
        }
        if uses.narration {
            used.push("the narration".into());
        }
        if uses.background {
            used.push("preview background".into());
        }
        ui.label(theme::caption("Used"));
        if used.is_empty() {
            ui.label(egui::RichText::new("not on the timeline yet").size(11.5).color(theme::MUTED));
        } else {
            ui.add(egui::Label::new(egui::RichText::new(used.join(" · ")).size(11.5).color(if uses.on_timeline() { theme::WHITE } else { theme::TEXT2 })).wrap());
        }
        ui.end_row();
    });

    ui.add_space(4.0);
    act
}

/// The panel's main action: a full-width white button with a plus.
fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(13.0), theme::BLACK);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
    let p = ui.painter();
    let hovered = resp.hovered();
    p.rect_filled(rect, 1.0, if hovered { theme::WHITE } else { egui::Color32::from_gray(0xe6) });
    if hovered {
        theme::glow_rect(p, rect, 0.14);
    }
    // The plus and the label, centred together.
    let total = 14.0 + galley.size().x;
    let x0 = rect.center().x - total / 2.0;
    let c = egui::pos2(x0 + 4.5, rect.center().y);
    let s = egui::Stroke::new(1.5_f32, theme::BLACK);
    p.line_segment([c - egui::vec2(4.5, 0.0), c + egui::vec2(4.5, 0.0)], s);
    p.line_segment([c - egui::vec2(0.0, 4.5), c + egui::vec2(0.0, 4.5)], s);
    p.galley(egui::pos2(x0 + 14.0, rect.center().y - galley.size().y / 2.0), galley, theme::BLACK);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small square × that closes the panel.
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    let p = ui.painter();
    let hovered = resp.hovered();
    if hovered {
        p.rect_filled(rect, 1.0, theme::HOVER);
    }
    p.rect_stroke(rect, 1.0, egui::Stroke::new(1.0_f32, if hovered { theme::FOCUS } else { theme::BORDER }), egui::StrokeKind::Inside);
    let s = egui::Stroke::new(1.2_f32, if hovered { theme::WHITE } else { theme::TEXT2 });
    let c = rect.center();
    p.line_segment([c + egui::vec2(-4.0, -4.0), c + egui::vec2(4.0, 4.0)], s);
    p.line_segment([c + egui::vec2(4.0, -4.0), c + egui::vec2(-4.0, 4.0)], s);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A 1px full-width rule.
fn rule(ui: &mut egui::Ui, color: egui::Color32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(r.x_range(), r.center().y, egui::Stroke::new(1.0_f32, color));
}

fn bytes(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n => format!("{} KB", (n / 1024).max(1)),
    }
}

/// A folder for the panel: the home folder as ~.
fn folder(p: &Path) -> String {
    let abs = canon(p);
    match std::env::var_os("HOME").map(PathBuf::from).and_then(|h| abs.strip_prefix(&h).ok().map(Path::to_path_buf)) {
        Some(r) => format!("~/{}", r.display()),
        None => abs.display().to_string(),
    }
}

fn bit_rate(b: u64) -> String {
    if b >= 1_000_000 { format!("{:.1} Mb/s", b as f64 / 1e6) } else { format!("{} kb/s", (b / 1000).max(1)) }
}

/// A card's length badge: 0:08, 3:41, 1:02:03.
fn clock(d: f64) -> String {
    let s = d.round().max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn fps(f: f64) -> String {
    if (f - f.round()).abs() < 0.01 { format!("{f:.0}") } else { format!("{f:.2}").trim_end_matches('0').to_string() }
}

/// A codec's everyday name.
fn codec(c: &str) -> String {
    match c {
        "h264" => "H.264".into(),
        "hevc" => "H.265".into(),
        "prores" => "ProRes".into(),
        "vorbis" => "Vorbis".into(),
        "opus" => "Opus".into(),
        // pcm_s16le → PCM 16-bit, pcm_f32le → PCM 32-bit float.
        c if c.starts_with("pcm_") => {
            let bits: String = c.chars().filter(char::is_ascii_digit).collect();
            let float = if c.starts_with("pcm_f") { " float" } else { "" };
            if bits.is_empty() { "PCM".into() } else { format!("PCM {bits}-bit{float}") }
        }
        c => c.to_uppercase(),
    }
}

/// An audio stream: "AAC · stereo · 48 kHz".
fn sound(a: &vg_media::AudioStream) -> String {
    let mut s = vec![codec(&a.codec)];
    match a.channels {
        Some(1) => s.push("mono".into()),
        Some(2) => s.push("stereo".into()),
        Some(n) => s.push(format!("{n} channels")),
        None => {}
    }
    if let Some(r) = a.sample_rate {
        s.push(if r % 1000 == 0 { format!("{} kHz", r / 1000) } else { format!("{:.1} kHz", r as f64 / 1000.0) });
    }
    s.join(" · ")
}
