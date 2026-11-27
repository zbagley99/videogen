//! The preview: the GPU canvas shown at the preset's aspect ratio over a chosen
//! background. Drag the avatar to move it; scroll over the preview to resize it.
//! The caption is a box of its own: drag it to move, pull its side handles to set
//! the wrap width, pull a corner (or scroll over it) to change the text size.
//! Where the video track has a clip, its frame fills the canvas under the avatar
//! (see `frames`); the chosen background shows between clips.

use super::util::file_name;
use super::frames::{self, Next, Shown, Want};
use super::{VgApp, theme};
use eframe::egui;
use std::path::PathBuf;
use vg_core::PoseSet;
use vg_render::{Compositor, frame_at};

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Background {
    Checker,
    Color,
    Image,
}

pub struct Preview {
    device: wgpu::Device,
    renderer: std::sync::Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    comp: Compositor,
    texture: egui::TextureId,
    bg_image: Option<(PathBuf, egui::TextureHandle)>,
    /// The caption font registered with egui: (name, pass it was added in).
    caption_font: Option<(String, u64)>,
    /// Drag in progress: what is being edited, the pointer at the start and the
    /// values it started from.
    drag: Option<Drag>,
    /// Height of the caption box in canvas pixels (the last caption drawn, one line
    /// if none yet), for hit-testing before this frame's caption is laid out.
    caption_h: f32,
    /// Grid lines the avatar is snapped to (fractions of the canvas), for highlighting.
    snapped: (Option<f32>, Option<f32>),
    /// An avatar drag: the pose being placed (None: the default spot) and the
    /// layout it started from (for Shift, which moves every pose).
    drag_pose: Option<String>,
    drag_from: Option<vg_core::AvatarLayout>,
    /// Frames of the video track.
    pub frames: frames::Frames,
}

impl Preview {
    pub fn new(rs: &egui_wgpu::RenderState, width: u32, height: u32) -> Self {
        let comp = Compositor::new(&rs.device, &rs.queue, width, height);
        let texture = rs.renderer.write().register_native_texture(&rs.device, comp.canvas_view(), wgpu::FilterMode::Linear);
        Preview { device: rs.device.clone(), renderer: rs.renderer.clone(), comp, texture, bg_image: None, caption_font: None, drag: None, caption_h: 0.0, snapped: (None, None), drag_pose: None, drag_from: None, frames: Default::default() }
    }

    pub fn load_poses(&mut self, poses: &PoseSet) -> anyhow::Result<()> {
        self.comp.load_poses(poses)
    }

    fn ensure_size(&mut self, w: u32, h: u32) {
        if self.comp.size() != (w, h) {
            self.comp.resize(w, h);
            self.renderer.write().update_egui_texture_from_wgpu_texture(&self.device, self.comp.canvas_view(), wgpu::FilterMode::Linear, self.texture);
        }
    }

    /// Register the caption style's font (found with fontconfig, bold) so the preview
    /// matches libass. Returns the family once egui has it (fonts apply a pass later).
    fn caption_family(&mut self, ctx: &egui::Context, font: &str) -> Option<egui::FontFamily> {
        let pass = ctx.cumulative_pass_nr();
        if self.caption_font.as_ref().map(|(n, _)| n.as_str()) != Some(font) {
            let file = std::process::Command::new("fc-match").args(["-f", "%{file}", &format!("{font}:bold")]).output().ok()?;
            let bytes = std::fs::read(String::from_utf8_lossy(&file.stdout).trim()).ok()?;
            ctx.add_font(egui::epaint::text::FontInsert::new(
                &format!("caption:{font}"),
                egui::FontData::from_owned(bytes),
                vec![egui::epaint::text::InsertFontFamily { family: egui::FontFamily::Name("caption".into()), priority: egui::epaint::text::FontPriority::Highest }],
            ));
            self.caption_font = Some((font.to_string(), pass));
        }
        let (_, added) = self.caption_font.as_ref()?;
        (pass > *added).then(|| egui::FontFamily::Name("caption".into()))
    }

    fn bg_texture(&mut self, ctx: &egui::Context, path: &PathBuf) -> Option<egui::TextureId> {
        if self.bg_image.as_ref().map(|(p, _)| p) != Some(path) {
            let img = image::open(path).ok()?.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            let handle = ctx.load_texture("preview-bg", color, egui::TextureOptions::LINEAR);
            self.bg_image = Some((path.clone(), handle));
        }
        self.bg_image.as_ref().map(|(_, h)| h.id())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Target {
    Avatar,
    CaptionMove,
    /// A side handle: -1 left, +1 right.
    CaptionWidth(f32),
    /// A corner handle.
    CaptionSize,
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    target: Target,
    start: egui::Pos2,
    /// Avatar (x, y) or caption (x, y).
    x: f32,
    y: f32,
    size: f32,
    /// Shift was held: move every pose.
    all: bool,
}

/// The caption box on screen: centred on (x, y), as wide as the wrap width and as
/// tall as the text.
fn caption_frame(rect: egui::Rect, style: &vg_core::CaptionStyle, text_h: f32) -> egui::Rect {
    let c = egui::pos2(rect.left() + style.x * rect.width(), rect.top() + style.y * rect.height());
    egui::Rect::from_center_size(c, egui::vec2(style.width * rect.width(), text_h.max(8.0)))
}

/// What the pointer at `p` would grab: a handle, the caption body, or the avatar.
fn hit(frame: Option<egui::Rect>, p: egui::Pos2) -> Target {
    let Some(f) = frame else { return Target::Avatar };
    const R: f32 = 8.0;
    if [f.left_top(), f.right_top(), f.left_bottom(), f.right_bottom()].iter().any(|c| c.distance(p) <= R) {
        return Target::CaptionSize;
    }
    for side in [-1.0_f32, 1.0] {
        let x = if side < 0.0 { f.left() } else { f.right() };
        if (p.x - x).abs() <= R && p.y >= f.top() - R && p.y <= f.bottom() + R {
            return Target::CaptionWidth(side);
        }
    }
    if f.expand(4.0).contains(p) { Target::CaptionMove } else { Target::Avatar }
}

fn cursor(t: Target, frame: Option<egui::Rect>, p: egui::Pos2) -> egui::CursorIcon {
    match t {
        Target::Avatar => egui::CursorIcon::Grab,
        Target::CaptionMove => egui::CursorIcon::Move,
        Target::CaptionWidth(_) => egui::CursorIcon::ResizeHorizontal,
        Target::CaptionSize => match frame {
            // Top-left/bottom-right corners resize along one diagonal, the others along the other.
            Some(f) if (p.x < f.center().x) == (p.y < f.center().y) => egui::CursorIcon::ResizeNwSe,
            _ => egui::CursorIcon::ResizeNeSw,
        },
    }
}

pub fn show(app: &mut VgApp, ui: &mut egui::Ui) {
    let preset = app.project.preset.clone();
    let (w, h) = (preset.width, preset.height);
    app.preview.ensure_size(w, h);

    // Toolbar.
    ui.horizontal(|ui| {
        theme::panel_title(ui, "Preview", &format!("{w}×{h} · {} fps", preset.fps));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if app.prefs.snap_canvas {
                let mut divs = app.prefs.grid_divs;
                theme::segmented(ui, &mut divs, &[(24, "24"), (12, "12"), (6, "6")]);
                app.prefs.grid_divs = divs;
            }
            if theme::tool_button(ui, "Grid", None, app.prefs.snap_canvas)
                .on_hover_text("Snap to grid · shows the grid and pulls the avatar's anchor onto its lines (edges, centre, thirds with 12 or 6) while you drag. Hold Alt to place freely.")
                .clicked()
            {
                app.prefs.snap_canvas = !app.prefs.snap_canvas;
            }
            if preset.height > preset.width
                && theme::tool_button(ui, "Safe area", None, app.prefs.show_guides)
                    .on_hover_text("Outline the part of a Short that YouTube's player UI leaves clear (an estimate).")
                    .clicked()
                {
                    app.prefs.show_guides = !app.prefs.show_guides;
                }
            if app.prefs.preview_bg == Background::Color {
                ui.color_edit_button_srgb(&mut app.prefs.preview_bg_color);
            }
            if app.prefs.preview_bg == Background::Image && theme::tool_button(ui, "Choose image", None, false).clicked() {
                super::dialog::open(app, super::dialog::Want::PreviewBg { revert: None }, rfd::FileDialog::new().add_filter("image", &["png", "jpg", "jpeg", "webp"]), super::dialog::How::File);
            }
            let before = app.prefs.preview_bg;
            theme::segmented(ui, &mut app.prefs.preview_bg, &[(Background::Image, "Image"), (Background::Color, "Colour"), (Background::Checker, "Checker")]);
            if app.prefs.preview_bg == Background::Image && before != Background::Image && app.prefs.preview_bg_image.is_none() {
                super::dialog::open(app, super::dialog::Want::PreviewBg { revert: Some(before) }, rfd::FileDialog::new().add_filter("image", &["png", "jpg", "jpeg", "webp"]), super::dialog::How::File);
            }
            // With clips on the video track, the background only fills the gaps between them.
            let label = if app.project.video.is_empty() { "Background" } else { "Background · between video clips" };
            ui.label(theme::caption(label).color(theme::MUTED));
        });
    });

    // Fit the canvas into the panel, leaving room for the frame ticks and the caption below.
    let avail = ui.available_rect_before_wrap().shrink2(egui::vec2(14.0, 10.0));
    let avail = egui::Rect::from_min_max(avail.min, egui::pos2(avail.max.x, avail.max.y - 44.0));
    let scale = (avail.width() / w as f32).min(avail.height() / h as f32).max(0.01);
    let size = egui::vec2((w as f32 * scale).round(), (h as f32 * scale).round());
    let rect = egui::Rect::from_center_size(avail.center(), size);
    let response = ui.allocate_rect(rect, egui::Sense::drag());
    let painter = ui.painter_at(rect);

    // The video track's clip under the playhead, fitted like a background; the
    // chosen background shows where there's none.
    let clip = app.project.video.at(app.time).map(|i| (i, app.project.video.clips[i].clone()));
    let mut video_note: Option<(String, egui::Color32)> = None;
    // Decode at the canvas's size on screen (even, and never above the export's).
    let ppp = ui.ctx().pixels_per_point();
    let px = |v: f32, max: u32| ((v * ppp / 2.0).round() as u32 * 2).clamp(2, max.max(2));
    let video_size = [px(rect.width(), w), px(rect.height(), h)];
    if let Some((i, clip)) = &clip {
        painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
        let path = super::video::source_path(app, clip);
        let src = clip.src_at(app.time).unwrap_or(clip.src_in);
        let next = app.project.video.clips.get(i + 1).filter(|n| app.playing && !n.still).map(|n| (super::video::source_path(app, n), n.src_in, n.start - app.time));
        let next = next.as_ref().map(|(p, src_in, secs)| Next { path: p, src_in: *src_in, secs: *secs, fps: preset.fps, size: video_size });
        let want = Want { path: &path, src, still: clip.still, fps: preset.fps, size: video_size, next };
        let name = file_name(&path).to_uppercase();
        let at = if clip.still { String::new() } else { format!(" · SRC {}", vg_core::format_time(src)) };
        match app.preview.frames.frame(ui.ctx(), &want, app.playing) {
            Shown::Frame { id, size, exact } => {
                // The fades go to black.
                let tint = egui::Color32::WHITE.gamma_multiply(clip.opacity_at(app.time));
                painter.image(id, rect, frames::cover_uv(size, rect.size()), tint);
                video_note = Some((format!("VIDEO {name}{at}{}", if exact { "" } else { " · LOADING" }), theme::MUTED));
            }
            Shown::Loading => video_note = Some((format!("VIDEO {name}{at} · LOADING"), theme::MUTED)),
            Shown::Missing => {
                painter.text(rect.center_top() + egui::vec2(0.0, 16.0), egui::Align2::CENTER_TOP, "MISSING VIDEO", egui::FontId::monospace(11.0), theme::DANGER);
                let galley = painter.layout(path.display().to_string(), egui::FontId::monospace(9.5), theme::MUTED, rect.width() - 24.0);
                painter.galley(egui::pos2(rect.left() + 12.0, rect.top() + 34.0), galley, theme::MUTED);
                video_note = Some((format!("VIDEO {name} · MISSING"), theme::DANGER));
            }
            Shown::Unreadable(e) => {
                painter.text(rect.center_top() + egui::vec2(0.0, 16.0), egui::Align2::CENTER_TOP, "UNREADABLE VIDEO", egui::FontId::monospace(11.0), theme::DANGER);
                // ffmpeg's last word on it is the useful one ("Invalid data found…").
                let why = e.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("").to_string();
                let galley = painter.layout(format!("{}\n{why}", path.display()), egui::FontId::monospace(9.5), theme::MUTED, rect.width() - 24.0);
                painter.galley(egui::pos2(rect.left() + 12.0, rect.top() + 34.0), galley, theme::MUTED);
                video_note = Some((format!("VIDEO {name} · UNREADABLE"), theme::DANGER));
            }
        }
    } else {
        // Between clips: the next one's stream starts before it's due.
        let next = app.project.video.clips.iter().find(|n| app.playing && !n.still && n.start > app.time).map(|n| (super::video::source_path(app, n), n.src_in, n.start - app.time));
        let next = next.as_ref().map(|(p, src_in, secs)| Next { path: p, src_in: *src_in, secs: *secs, fps: preset.fps, size: video_size });
        app.preview.frames.idle(ui.ctx(), next);
    }
    match app.prefs.preview_bg {
        _ if clip.is_some() => {}
        Background::Checker => {
            // A quiet checker, anchored to the canvas so it scales with it.
            let cell = (rect.width() / 18.0).max(8.0);
            painter.rect_filled(rect, 0.0, egui::Color32::from_gray(0x0b));
            let (nx, ny) = ((rect.width() / cell).ceil() as i32, (rect.height() / cell).ceil() as i32);
            for y in 0..ny {
                for x in (y % 2..nx).step_by(2) {
                    let r = egui::Rect::from_min_size(rect.min + egui::vec2(x as f32 * cell, y as f32 * cell), egui::vec2(cell, cell));
                    painter.rect_filled(r.intersect(rect), 0.0, egui::Color32::from_gray(0x16));
                }
            }
        }
        Background::Color => {
            let [r, g, b] = app.prefs.preview_bg_color;
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(r, g, b));
        }
        Background::Image => {
            painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
            if let Some(path) = app.prefs.preview_bg_image.clone()
                && let Some(id) = app.preview.bg_texture(ui.ctx(), &path) {
                    painter.image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
        }
    }

    // Render and show the avatar frame.
    let frame = match &app.parsed.timeline {
        Some(tl) => frame_at(tl, &preset, &app.poses, app.time),
        None => Default::default(),
    };
    let hidden = frame.avatar.is_none() && app.parsed.timeline.is_some();
    // Each pose has its own spot: the canvas moves the one on screen.
    let shown: Option<String> = frame.avatar.and_then(|(p, _)| app.poses.poses.get(p)).map(|p| p.tag.clone());
    app.preview.comp.draw(&frame);
    painter.image(app.preview.texture, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);

    // The caption box: what the pointer is over (or dragging) decides the cursor,
    // the outline and whether an off-screen caption is shown as a ghost.
    let style = &preset.captions;
    if app.preview.caption_h <= 0.0 {
        app.preview.caption_h = style.size * h as f32 * 1.25;
    }
    let frame = style.enabled.then(|| caption_frame(rect, style, app.preview.caption_h * scale));
    let hover = response.hover_pos().map(|p| (hit(frame, p), p));
    let active = app.preview.drag.map(|d| d.target).or(hover.map(|h| h.0));
    let editing_caption = active.is_some_and(|t| t != Target::Avatar);

    if style.enabled {
        let chunks = match &app.parsed.timeline {
            Some(tl) => {
                let words: &[vg_core::Word] = app.words.as_ref().map(|w| w.2.as_slice()).unwrap_or(&[]);
                vg_core::captions::chunks(tl, words, style)
            }
            None => vec![],
        };
        let family = app.preview.caption_family(ui.ctx(), &style.font).unwrap_or(egui::FontFamily::Proportional);
        let drawn = match vg_core::captions::caption_at(&chunks, app.time) {
            Some((chunk, hl)) => Some(draw_caption(&painter, rect, scale, &preset, chunk, hl, app.time, family)),
            None if editing_caption => {
                // Nothing spoken right now: show the nearest caption faintly so
                // there is something to place.
                let t = app.time;
                let sample = chunks.iter().min_by(|a, b| (a.start - t).abs().max((a.end - t).abs()).total_cmp(&(b.start - t).abs().max((b.end - t).abs()))).cloned();
                let sample = sample.unwrap_or_else(|| vg_core::captions::Chunk {
                    start: 0.0,
                    end: 1.0,
                    words: ["Caption", "goes", "here"].iter().map(|w| vg_core::captions::CaptionWord { text: if style.uppercase { w.to_uppercase() } else { w.to_string() }, start: 0.0 }).collect(),
                });
                let mut ghost = painter.clone();
                ghost.multiply_opacity(0.45);
                Some(draw_caption(&ghost, rect, scale, &preset, &sample, usize::MAX, sample.start + 1.0, family))
            }
            None => None,
        };
        if let Some(text_h) = drawn {
            app.preview.caption_h = text_h / scale;
        }
    }

    if app.prefs.snap_canvas {
        let divs = app.prefs.grid_divs.max(1);
        for k in 1..divs {
            let f = k as f32 / divs as f32;
            let centre = k * 2 == divs;
            let base = egui::Color32::from_white_alpha(if centre { 46 } else { 20 });
            let (sx, sy) = app.preview.snapped;
            let lit = |s: Option<f32>| s.is_some_and(|v| (v - f).abs() < 1e-4);
            let x = rect.left() + f * rect.width();
            let y = rect.top() + f * rect.height();
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0_f32, if lit(sx) { egui::Color32::from_white_alpha(190) } else { base }));
            painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(1.0_f32, if lit(sy) { egui::Color32::from_white_alpha(190) } else { base }));
        }
    }

    if app.prefs.show_guides && preset.height > preset.width {
        // Estimated clear area of a Short: the top bar, the button column on the
        // right and the title/channel block at the bottom cover the rest.
        let safe = egui::Rect::from_min_max(
            egui::pos2(rect.left() + rect.width() * 0.06, rect.top() + rect.height() * 0.11),
            egui::pos2(rect.right() - rect.width() * 0.16, rect.bottom() - rect.height() * 0.22),
        );
        theme::dashed_rect(&painter, safe, egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(110)));
        painter.text(safe.left_top() + egui::vec2(4.0, 4.0), egui::Align2::LEFT_TOP, "SAFE AREA*", egui::FontId::monospace(9.5), egui::Color32::from_white_alpha(150));
    }

    if frame.is_some() {
        // Faint while the pointer is anywhere on the canvas, bright with handles
        // while it is on the caption (or dragging it).
        if editing_caption || response.hovered() {
            let alpha = if editing_caption { 200 } else { 60 };
            let stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(alpha));
            let f = caption_frame(rect, style, app.preview.caption_h * scale);
            theme::dashed_rect(&painter, f, stroke);
            if editing_caption {
                let handles = [f.left_top(), f.right_top(), f.left_bottom(), f.right_bottom(), f.left_center(), f.right_center()];
                for (i, c) in handles.into_iter().enumerate() {
                    let r = egui::Rect::from_center_size(c, egui::vec2(7.0, if i >= 4 { 13.0 } else { 7.0 }));
                    painter.rect_filled(r, 0.0, theme::BLACK);
                    painter.rect_stroke(r, 0.0, egui::Stroke::new(1.0_f32, theme::WHITE), egui::StrokeKind::Inside);
                }
                let label = format!("CAPTION  X {:.2}  Y {:.2}  SIZE {:.1}%  WRAP {:.0}%", style.x, style.y, style.size * 100.0, style.width * 100.0);
                let at = if f.top() > rect.top() + 18.0 { (f.left_top() - egui::vec2(0.0, 5.0), egui::Align2::LEFT_BOTTOM) } else { (f.left_bottom() + egui::vec2(0.0, 5.0), egui::Align2::LEFT_TOP) };
                let galley = painter.layout_no_wrap(label, egui::FontId::monospace(9.5), theme::WHITE);
                let mut bg = at.1.anchor_size(at.0, galley.size()).expand(2.0);
                // Keep the readout on the canvas when the box hangs off an edge.
                let dx = if bg.right() > rect.right() { rect.right() - bg.right() } else { 0.0 };
                bg = bg.translate(egui::vec2(dx.max(rect.left() - bg.left()), 0.0));
                painter.rect_filled(bg, 0.0, egui::Color32::from_black_alpha(200));
                painter.galley(bg.min + egui::vec2(2.0, 2.0), galley, theme::WHITE);
            }
        }
    }

    // The frame: a crisp edge, drawing ticks at the corners, and the dimensions below.
    let p = ui.painter();
    p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Outside);
    theme::corner_ticks(p, rect, 10.0, egui::Stroke::new(1.0_f32, theme::FOCUS));
    let aspect = if w > h { "16:9" } else if h > w { "9:16" } else { "1:1" };
    p.text(
        egui::pos2(rect.left(), rect.bottom() + 10.0),
        egui::Align2::LEFT_TOP,
        format!("FIG. 01 / {} · {aspect}", preset.name.to_uppercase()),
        egui::FontId::monospace(10.0),
        theme::MUTED,
    );
    p.text(egui::pos2(rect.right(), rect.bottom() + 10.0), egui::Align2::RIGHT_TOP, format!("T {}", vg_core::format_time(app.time)), egui::FontId::monospace(10.0), theme::MUTED);

    let hint = match active {
        Some(Target::CaptionMove) => "CAPTION · DRAG MOVE · SCROLL SIZE · DOUBLE-CLICK CENTRE · ALT NO SNAP",
        Some(Target::CaptionWidth(_)) => "CAPTION · DRAG WRAP WIDTH · ALT NO SNAP",
        Some(Target::CaptionSize) => "CAPTION · DRAG TEXT SIZE",
        Some(Target::Avatar) => "AVATAR · DRAG MOVE · SCROLL SIZE · ALT NO SNAP",
        None => "",
    };
    if !hint.is_empty() {
        let galley = p.layout(hint.to_string(), egui::FontId::monospace(9.5), theme::TEXT2, rect.width());
        p.galley(egui::pos2(rect.left(), rect.bottom() + 23.0), galley, theme::TEXT2);
    } else if let Some((note, color)) = video_note {
        // Otherwise the same line says which clip is showing, and the moment of its source.
        let galley = p.layout(note, egui::FontId::monospace(9.5), color, rect.width());
        p.galley(egui::pos2(rect.left(), rect.bottom() + 23.0), galley, color);
    }

    if app.poses.poses.is_empty() {
        painter.text(rect.center() - egui::vec2(0.0, 9.0), egui::Align2::CENTER_CENTER, "NO POSE LIBRARY", egui::FontId::monospace(12.0), theme::WHITE);
        painter.text(rect.center() + egui::vec2(0.0, 9.0), egui::Align2::CENTER_CENTER, "File › New pose library", egui::FontId::monospace(10.5), theme::MUTED);
    } else if hidden {
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, "AVATAR HIDDEN", egui::FontId::monospace(11.0), theme::MUTED);
    }

    // Direct manipulation of the layout.
    if response.drag_started()
        && let Some(p) = response.interact_pointer_pos() {
            let target = hit(frame, p);
            let (a, c) = (&app.project.preset.avatar, &app.project.preset.captions);
            let all = ui.input(|i| i.modifiers.shift);
            let spot = match (&shown, all) {
                (Some(tag), false) => a.spot(tag),
                _ => a.default_spot(),
            };
            app.preview.drag_pose = if all { None } else { shown.clone() };
            app.preview.drag_from = Some(a.clone());
            app.preview.drag = Some(match target {
                Target::Avatar => Drag { target, start: p, x: spot.x, y: spot.y, size: spot.height, all },
                _ => Drag { target, start: p, x: c.x, y: c.y, size: c.size, all: false },
            });
        }
    app.preview.snapped = (None, None);
    let snapping = app.prefs.snap_canvas && !ui.input(|i| i.modifiers.alt);
    // Pull onto a grid line within 10 screen pixels (edges included).
    let divs = app.prefs.grid_divs.max(1) as f32;
    let pull = |v: f32, extent: f32| {
        let g = (v * divs).round() / divs;
        if snapping && ((v - g) * extent).abs() <= 10.0 { (g, Some(g)) } else { (v, None) }
    };
    if let (true, Some(drag), Some(p)) = (response.dragged(), app.preview.drag, response.interact_pointer_pos()) {
        let d = (p - drag.start) / scale;
        match drag.target {
            Target::Avatar | Target::CaptionMove => {
                let (x, hx) = pull(drag.x + d.x / w as f32, rect.width());
                let (y, hy) = pull(drag.y + d.y / h as f32, rect.height());
                app.preview.snapped = (hx, hy);
                if drag.target == Target::Avatar {
                    let (x, y) = (x.clamp(-0.5, 1.5), y.clamp(-0.5, 1.5));
                    let layout = &mut app.project.preset.avatar;
                    match (&app.preview.drag_pose, &app.preview.drag_from) {
                        (_, Some(from)) if drag.all => {
                            // Shift: every pose moves together, keeping their spacing.
                            let (dx, dy) = (x - drag.x, y - drag.y);
                            *layout = from.clone();
                            layout.x += dx;
                            layout.y += dy;
                            for s in layout.poses.values_mut() {
                                s.x += dx;
                                s.y += dy;
                            }
                        }
                        (Some(tag), _) => {
                            let s = layout.spot_mut(tag);
                            s.x = x;
                            s.y = y;
                        }
                        _ => {
                            layout.x = x;
                            layout.y = y;
                        }
                    }
                } else {
                    let c = &mut app.project.preset.captions;
                    c.x = x.clamp(0.0, 1.0);
                    c.y = y.clamp(0.0, 1.0);
                }
            }
            Target::CaptionWidth(side) => {
                // Symmetric about the centre; the dragged edge snaps to the grid.
                let c = &mut app.project.preset.captions;
                let (edge, hx) = pull((p.x - rect.left()) / rect.width(), rect.width());
                app.preview.snapped = (hx, None);
                let half = (edge - c.x) * side;
                c.width = (half * 2.0).clamp(0.1, 1.0);
            }
            Target::CaptionSize => {
                // Scale with the corner's distance from the centre line.
                let cy = rect.top() + app.project.preset.captions.y * rect.height();
                let from = (drag.start.y - cy).abs().max(4.0);
                let c = &mut app.project.preset.captions;
                c.size = (drag.size * (p.y - cy).abs().max(2.0) / from).clamp(0.01, 0.2);
            }
        }
    }
    if response.drag_stopped() {
        app.preview.drag = None;
        app.preview.drag_from = None;
    }
    // Say what the drag is moving.
    if let Some(d) = app.preview.drag.filter(|d| d.target == Target::Avatar) {
        let what = match (&app.preview.drag_pose, d.all) {
            (_, true) => "ALL POSES".to_string(),
            (Some(tag), _) => format!("{} · Shift moves all poses", tag.to_uppercase()),
            _ => "DEFAULT SPOT".to_string(),
        };
        let at = rect.left_top() + egui::vec2(8.0, 8.0);
        let galley = painter.layout_no_wrap(what, egui::FontId::monospace(10.0), theme::WHITE);
        painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand2(egui::vec2(6.0, 3.0)), 0.0, theme::BLACK);
        painter.galley(at, galley, theme::WHITE);
    }
    if response.double_clicked() && response.interact_pointer_pos().is_some_and(|p| hit(frame, p) == Target::CaptionMove) {
        app.project.preset.captions.x = 0.5;
    }
    if let Some((target, p)) = hover {
        // (egui turns Shift+wheel into a sideways scroll.)
        let scroll = ui.input(|i| if i.modifiers.shift { i.smooth_scroll_delta.x + i.smooth_scroll_delta.y } else { i.smooth_scroll_delta.y });
        if scroll != 0.0 {
            let k = 1.0 + scroll * 0.002;
            if target == Target::Avatar {
                let layout = &mut app.project.preset.avatar;
                let size = |h: &mut f32| *h = (*h * k).clamp(0.03, 3.0);
                match &shown {
                    _ if ui.input(|i| i.modifiers.shift) => {
                        size(&mut layout.height);
                        layout.poses.values_mut().for_each(|s| size(&mut s.height));
                    }
                    Some(tag) => size(&mut layout.spot_mut(tag).height),
                    None => size(&mut layout.height),
                }
            } else {
                let c = &mut app.project.preset.captions;
                c.size = (c.size * k).clamp(0.01, 0.2);
            }
        }
        let t = app.preview.drag.map(|d| d.target).unwrap_or(target);
        ui.ctx().set_cursor_icon(if t == Target::Avatar && app.preview.drag.is_some() { egui::CursorIcon::Grabbing } else { cursor(t, frame, p) });
    }
}

/// Approximates the libass rendering: same size, position, colours, outline and pop.
/// Returns the text's height without the pop, in screen pixels.
#[allow(clippy::too_many_arguments)]
fn draw_caption(painter: &egui::Painter, rect: egui::Rect, scale: f32, preset: &vg_core::Preset, chunk: &vg_core::captions::Chunk, hl: usize, t: f64, family: egui::FontFamily) -> f32 {
    let style = &preset.captions;
    let tau = (t - chunk.start) as f32;
    let pop = if style.pop > 0.0 && tau < 0.09 { 1.0 + style.pop * (1.0 - tau / 0.09) } else { 1.0 };
    let size = style.size * preset.height as f32 * scale * pop;
    // libass sizes a font by its ascent + descent; egui by its em. Measure the row
    // height egui gives and scale so a line is `size` tall, like the export.
    let probe = painter.layout_no_wrap("X".into(), egui::FontId::new(100.0, family.clone()), egui::Color32::WHITE).size().y;
    let font = egui::FontId::new(if probe > 0.0 { size * 100.0 / probe } else { size }, family.clone());
    let rgb = |c: u32| egui::Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
    let layout = |outline: bool| {
        let mut job = egui::text::LayoutJob::default();
        for (i, w) in chunk.words.iter().enumerate() {
            let color = if outline { egui::Color32::BLACK } else if i == hl { rgb(style.highlight) } else { rgb(style.color) };
            let text = if i + 1 < chunk.words.len() { format!("{} ", w.text) } else { w.text.clone() };
            job.append(&text, 0.0, egui::TextFormat { font_id: font.clone(), color, ..Default::default() });
        }
        // libass never breaks inside a word; a word wider than the wrap overflows.
        let widest = chunk.words.iter().map(|w| painter.layout_no_wrap(w.text.clone(), font.clone(), egui::Color32::WHITE).size().x).fold(0.0, f32::max);
        job.wrap.max_width = (rect.width() * style.width.clamp(0.05, 1.0)).max(widest + 1.0);
        job.halign = egui::Align::Center;
        painter.layout_job(job)
    };
    let center = egui::pos2(rect.left() + style.x * rect.width(), rect.top() + style.y * rect.height());
    let main = layout(false);
    let height = main.size().y / pop;
    let pos = center - egui::vec2(0.0, main.size().y / 2.0);
    let o = (style.outline * size).max(0.0);
    if o > 0.0 {
        let outline = layout(true);
        for k in 0..12 {
            let a = k as f32 / 12.0 * std::f32::consts::TAU;
            painter.galley(pos + egui::vec2(a.cos(), a.sin()) * o, outline.clone(), egui::Color32::BLACK);
        }
    }
    painter.galley(pos, main, egui::Color32::WHITE);
    height
}
