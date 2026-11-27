//! The look: after the Botslop system. A true-black field with thin white
//! structure, square 1px edges, uppercase tracked captions, and one accent:
//! phosphor amber (#ffb000), which here marks poses, the character's own colour.
//! Text is set in IBM Plex Sans for reading; IBM Plex Mono (the system's face)
//! stays for timecodes, numbers, paths, keys and the script source.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Stroke, StrokeKind, TextStyle, pos2, vec2};

pub const BLACK: Color32 = Color32::BLACK;
pub const WHITE: Color32 = Color32::WHITE;
/// Supporting text, labels, non-current prose (10.9:1).
pub const TEXT2: Color32 = Color32::from_gray(0xbb);
/// Hints, placeholders, meta (4.7:1). Never the only carrier of an instruction.
pub const MUTED: Color32 = Color32::from_gray(0x77);
/// Essential control edges (≥ 3:1).
pub const BORDER: Color32 = Color32::from_gray(0x73);
/// Hovered / focused control edges, window frames.
pub const FOCUS: Color32 = Color32::from_gray(0xc9);
/// Decorative rules and panel edges.
pub const RULE: Color32 = Color32::from_gray(0x42);
/// Quiet separators.
pub const RULE_SOFT: Color32 = Color32::from_gray(0x22);
/// Hover wash (always with an edge).
pub const HOVER: Color32 = Color32::from_gray(0x0f);
/// The one accent: poses.
pub const MARK: Color32 = Color32::from_rgb(0xff, 0xb0, 0x00);
/// Destructive actions only (delete buttons).
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x4d, 0x4d);

const MONO: &str = "plex-mono";
const SANS: &str = "plex-sans";

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(MONO.into(), std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf"))));
    fonts.font_data.insert(SANS.into(), std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf"))));
    // Plex first; egui's defaults stay behind it for symbols Plex lacks.
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, SANS.into());
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, MONO.into());
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Body, FontId::proportional(13.0)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
            (TextStyle::Heading, FontId::proportional(15.0)),
        ]
        .into();
        // Stillness: UI changes are near-instant.
        style.animation_time = 0.06;
        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 6.0);
        s.button_padding = vec2(10.0, 4.0);
        s.interact_size = vec2(24.0, 22.0);
        s.window_margin = egui::Margin::same(14);
        s.menu_margin = egui::Margin::same(6);
        s.slider_rail_height = 2.0;
        s.combo_width = 120.0;
        // Thin bars on the black field: a rule-grey handle, brighter under the
        // pointer, white while dragged (the system's `scrollbar-color: rule black`).
        s.scroll = egui::style::ScrollStyle {
            floating: true,
            bar_width: 6.0,
            floating_width: 6.0,
            floating_allocated_width: 8.0,
            handle_min_length: 24.0,
            bar_inner_margin: 2.0,
            bar_outer_margin: 0.0,
            foreground_color: true,
            dormant_background_opacity: 1.0,
            active_background_opacity: 1.0,
            interact_background_opacity: 1.0,
            dormant_handle_opacity: 0.36,
            active_handle_opacity: 0.5,
            interact_handle_opacity: 0.85,
            ..egui::style::ScrollStyle::solid()
        };

        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = BLACK;
        v.window_fill = BLACK;
        v.extreme_bg_color = BLACK;
        v.faint_bg_color = Color32::from_gray(0x08);
        v.code_bg_color = Color32::from_gray(0x12);
        v.window_stroke = Stroke::new(1.0_f32, FOCUS);
        v.window_corner_radius = CornerRadius::same(1);
        v.menu_corner_radius = CornerRadius::same(1);
        v.window_shadow = egui::Shadow { offset: [0, 0], blur: 18, spread: 0, color: Color32::from_white_alpha(14) };
        v.popup_shadow = egui::Shadow { offset: [0, 0], blur: 14, spread: 0, color: Color32::from_white_alpha(12) };
        v.hyperlink_color = WHITE;
        v.warn_fg_color = MARK;
        v.error_fg_color = WHITE;
        v.selection.bg_fill = Color32::from_gray(0x3a);
        v.selection.stroke = Stroke::new(1.0_f32, WHITE);
        v.text_cursor.stroke = Stroke::new(1.5_f32, WHITE);
        v.text_cursor.blink = false;
        v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.55 };
        v.slider_trailing_fill = true;
        v.indent_has_left_vline = false;

        let r = CornerRadius::same(1);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BLACK;
        w.noninteractive.weak_bg_fill = BLACK;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, RULE_SOFT);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT2);
        w.noninteractive.corner_radius = r;
        w.inactive.bg_fill = Color32::from_gray(0x1c);
        w.inactive.weak_bg_fill = BLACK;
        w.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
        w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT2);
        w.inactive.corner_radius = r;
        w.hovered.bg_fill = Color32::from_gray(0x2a);
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0_f32, FOCUS);
        w.hovered.fg_stroke = Stroke::new(1.0_f32, WHITE);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;
        w.active.bg_fill = WHITE;
        w.active.weak_bg_fill = Color32::from_gray(0x16);
        w.active.bg_stroke = Stroke::new(1.0_f32, WHITE);
        w.active.fg_stroke = Stroke::new(1.0_f32, WHITE);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;
        w.open.bg_fill = BLACK;
        w.open.weak_bg_fill = HOVER;
        w.open.bg_stroke = Stroke::new(1.0_f32, FOCUS);
        w.open.fg_stroke = Stroke::new(1.0_f32, WHITE);
        w.open.corner_radius = r;
    });
}

// ---- text -------------------------------------------------------------------

/// Uppercase, tracked, small: labels and section captions.
pub fn caption(text: impl AsRef<str>) -> egui::RichText {
    egui::RichText::new(text.as_ref().to_uppercase()).size(10.5).extra_letter_spacing(1.0).color(TEXT2)
}

/// A panel title: `POSES · 20`, with the meta part muted.
pub fn panel_title(ui: &mut egui::Ui, title: &str, meta: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new(title.to_uppercase()).size(12.0).extra_letter_spacing(2.0).color(WHITE));
        if !meta.is_empty() {
            ui.label(egui::RichText::new(format!("· {meta}")).size(11.0).color(MUTED));
        }
    });
}

/// Quiet secondary text (hints, explanations), wrapped to the width.
pub fn weak(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.add(egui::Label::new(egui::RichText::new(text.into()).size(10.5).color(MUTED)).wrap());
}

/// A quiet one-line note, a touch larger than [`weak`].
pub fn note(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(egui::RichText::new(text.into()).size(11.0).color(MUTED));
}

// ---- controls ---------------------------------------------------------------

/// A keyboard chip: the key name in a small bordered box.
pub fn key_chip(ui: &mut egui::Ui, key: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(key.to_string(), FontId::monospace(9.5), TEXT2);
    let size = galley.size() + vec2(8.0, 3.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_stroke(rect, 1.0, Stroke::new(1.0_f32, BORDER), StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, TEXT2);
    resp
}

/// A bordered text button with an optional key chip; `on` draws it lit (inverted).
pub fn tool_button(ui: &mut egui::Ui, label: &str, key: Option<&str>, on: bool) -> egui::Response {
    tool_button_enabled(ui, label, key, on, true)
}

pub fn tool_button_enabled(ui: &mut egui::Ui, label: &str, key: Option<&str>, on: bool, enabled: bool) -> egui::Response {
    let font = FontId::proportional(12.5);
    let text_col = if on { BLACK } else if enabled { WHITE } else { MUTED };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, text_col);
    let key_galley = key.map(|k| ui.painter().layout_no_wrap(k.to_string(), FontId::monospace(9.5), if on { Color32::from_gray(0x33) } else { MUTED }));
    let key_w = key_galley.as_ref().map(|g| g.size().x + 8.0 + 8.0).unwrap_or(0.0);
    let size = vec2(galley.size().x + 20.0 + key_w, 24.0);
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let p = ui.painter();
    let hovered = enabled && resp.hovered();
    if on {
        p.rect_filled(rect, 1.0, WHITE);
    } else {
        if hovered {
            p.rect_filled(rect, 1.0, HOVER);
            glow_rect(p, rect, 0.10);
        }
        let edge = if !enabled { RULE } else if hovered { FOCUS } else { BORDER };
        p.rect_stroke(rect, 1.0, Stroke::new(1.0_f32, edge), StrokeKind::Inside);
    }
    let gp = pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0);
    p.galley(gp, galley.clone(), text_col);
    if let Some(kg) = key_galley {
        let kr = Rect::from_min_size(pos2(gp.x + galley.size().x + 8.0, rect.center().y - kg.size().y / 2.0 - 1.5), kg.size() + vec2(8.0, 3.0));
        p.rect_stroke(kr, 1.0, Stroke::new(1.0_f32, if on { Color32::from_gray(0x99) } else { RULE }), StrokeKind::Inside);
        p.galley(kr.center() - kg.size() / 2.0, kg, MUTED);
    }
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// A colour picker for a `0xRRGGBB` value.
pub fn color_u32(ui: &mut egui::Ui, c: &mut u32) {
    let mut rgb = [(*c >> 16) as u8, (*c >> 8) as u8, *c as u8];
    if ui.color_edit_button_srgb(&mut rgb).changed() {
        *c = (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32;
    }
}

/// A red button for something that can't be undone: red outline and text,
/// filled red while hovered.
pub fn danger_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(12.5), DANGER);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 20.0, 24.0), egui::Sense::click());
    let p = ui.painter();
    let hovered = resp.hovered();
    if hovered {
        p.rect_filled(rect, 1.0, DANGER);
    } else {
        p.rect_stroke(rect, 1.0, Stroke::new(1.0_f32, DANGER), StrokeKind::Inside);
    }
    p.galley(pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0), galley, if hovered { BLACK } else { DANGER });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row of options where the chosen one is inverted (white fill, black text).
pub fn segmented<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (v, label) in options {
            let on = *value == *v;
            let font = FontId::proportional(11.0);
            let galley = ui.painter().layout_no_wrap(label.to_uppercase(), font, if on { BLACK } else { TEXT2 });
            let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 18.0, 22.0), egui::Sense::click());
            let p = ui.painter();
            if on {
                p.rect_filled(rect, 0.0, WHITE);
            } else if resp.hovered() {
                p.rect_filled(rect, 0.0, HOVER);
            }
            p.rect_stroke(rect, 0.0, Stroke::new(1.0_f32, if on { WHITE } else { BORDER }), StrokeKind::Inside);
            p.galley(rect.center() - galley.size() / 2.0, galley, if on { BLACK } else { TEXT2 });
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                *value = v.clone();
                changed = true;
            }
        }
    });
    changed
}

/// Tabs across the top of a panel: uppercase labels with a count, the chosen
/// one white and underlined, over a rule the width of the panel.
pub fn tabs<T: PartialEq + Clone>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str, String)]) -> bool {
    let mut changed = false;
    let top = ui.cursor().top();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        for (v, label, meta) in options {
            let on = *value == *v;
            let mut job = egui::text::LayoutJob::default();
            let fmt = |size: f32, color: Color32, spacing: f32| egui::TextFormat { font_id: FontId::proportional(size), color, extra_letter_spacing: spacing, valign: egui::Align::Center, ..Default::default() };
            job.append(&label.to_uppercase(), 0.0, fmt(12.0, if on { WHITE } else { TEXT2 }, 2.0));
            if !meta.is_empty() {
                job.append(meta, 6.0, fmt(11.0, MUTED, 0.0));
            }
            let galley = ui.painter().layout_job(job);
            let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x, 26.0), egui::Sense::click());
            let hovered = resp.hovered();
            let p = ui.painter();
            p.galley(pos2(rect.left(), rect.center().y - galley.size().y / 2.0 - 1.0), galley, TEXT2);
            if on || hovered {
                let y = rect.bottom() - 1.0;
                p.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(2.0_f32, if on { WHITE } else { BORDER }));
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                *value = v.clone();
                changed = true;
            }
        }
    });
    let r = ui.max_rect();
    let y = top + 26.0;
    ui.painter().line_segment([pos2(r.left(), y), pos2(r.right(), y)], Stroke::new(1.0_f32, RULE));
    ui.add_space(4.0);
    changed
}

#[derive(Clone, Copy)]
pub enum Icon {
    Play,
    Pause,
    ToStart,
    Speaker,
    Muted,
    Fit,
}

/// A square line-icon button.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), egui::Sense::click());
    let hovered = resp.hovered();
    let p = ui.painter();
    if hovered {
        p.rect_filled(rect, 1.0, HOVER);
        glow_rect(p, rect, 0.10);
    }
    p.rect_stroke(rect, 1.0, Stroke::new(1.0_f32, if hovered { FOCUS } else { BORDER }), StrokeKind::Inside);
    let c = rect.center();
    let s = Stroke::new(1.2_f32, if hovered { WHITE } else { TEXT2 });
    match icon {
        Icon::Play => {
            let pts = vec![c + vec2(-3.5, -5.5), c + vec2(5.0, 0.0), c + vec2(-3.5, 5.5)];
            p.add(egui::Shape::closed_line(pts, s));
        }
        Icon::Pause => {
            for dx in [-3.0, 3.0] {
                p.rect_stroke(Rect::from_center_size(c + vec2(dx, 0.0), vec2(2.5, 10.0)), 0.0, s, StrokeKind::Middle);
            }
        }
        Icon::ToStart => {
            p.line_segment([c + vec2(-5.0, -5.5), c + vec2(-5.0, 5.5)], s);
            p.add(egui::Shape::closed_line(vec![c + vec2(4.5, -5.5), c + vec2(-3.0, 0.0), c + vec2(4.5, 5.5)], s));
        }
        Icon::Speaker | Icon::Muted => {
            let body = vec![c + vec2(-6.0, -2.5), c + vec2(-3.0, -2.5), c + vec2(1.0, -6.0), c + vec2(1.0, 6.0), c + vec2(-3.0, 2.5), c + vec2(-6.0, 2.5)];
            p.add(egui::Shape::closed_line(body, s));
            if matches!(icon, Icon::Muted) {
                p.line_segment([c + vec2(3.5, -3.0), c + vec2(8.0, 3.0)], s);
                p.line_segment([c + vec2(3.5, 3.0), c + vec2(8.0, -3.0)], s);
            } else {
                p.line_segment([c + vec2(4.0, -3.0), c + vec2(5.5, 0.0)], s);
                p.line_segment([c + vec2(5.5, 0.0), c + vec2(4.0, 3.0)], s);
                p.line_segment([c + vec2(6.5, -5.0), c + vec2(8.5, 0.0)], s);
                p.line_segment([c + vec2(8.5, 0.0), c + vec2(6.5, 5.0)], s);
            }
        }
        Icon::Fit => {
            let r = Rect::from_center_size(c, vec2(12.0, 8.0));
            for (a, d) in [(r.left_top(), vec2(1.0, 1.0)), (r.right_top(), vec2(-1.0, 1.0)), (r.left_bottom(), vec2(1.0, -1.0)), (r.right_bottom(), vec2(-1.0, -1.0))] {
                p.line_segment([a, a + vec2(3.0 * d.x, 0.0)], s);
                p.line_segment([a, a + vec2(0.0, 3.0 * d.y)], s);
            }
        }
    }
    resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

// ---- drawing ----------------------------------------------------------------

/// A faint outer halo around a rect: light is local.
pub fn glow_rect(p: &egui::Painter, rect: Rect, strength: f32) {
    for (grow, a) in [(1.0, 0.55), (3.0, 0.25), (6.0, 0.10)] {
        let alpha = (255.0 * strength * a) as u8;
        p.rect_stroke(rect.expand(grow), 1.0, Stroke::new(1.5_f32, Color32::from_white_alpha(alpha)), StrokeKind::Outside);
    }
}

/// A 1px line with a soft halo.
pub fn glow_line(p: &egui::Painter, a: Pos2, b: Pos2, color: Color32) {
    p.line_segment([a, b], Stroke::new(5.0_f32, color.gamma_multiply(0.10)));
    p.line_segment([a, b], Stroke::new(3.0_f32, color.gamma_multiply(0.20)));
    p.line_segment([a, b], Stroke::new(1.0_f32, color));
}

pub fn dashed_rect(p: &egui::Painter, rect: Rect, stroke: Stroke) {
    let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
    for w in pts.windows(2) {
        p.extend(egui::Shape::dashed_line(&[w[0], w[1]], stroke, 5.0, 4.0));
    }
}

/// Engineering-drawing corner ticks just outside a frame.
pub fn corner_ticks(p: &egui::Painter, rect: Rect, len: f32, stroke: Stroke) {
    let r = rect.expand(4.0);
    for (corner, d) in [(r.left_top(), vec2(1.0, 1.0)), (r.right_top(), vec2(-1.0, 1.0)), (r.left_bottom(), vec2(1.0, -1.0)), (r.right_bottom(), vec2(-1.0, -1.0))] {
        p.line_segment([corner, corner + vec2(len * d.x, 0.0)], stroke);
        p.line_segment([corner, corner + vec2(0.0, len * d.y)], stroke);
    }
}

/// The amber tick in a card's top-right corner: what's in use right now.
pub fn marked_corner(p: &egui::Painter, rect: Rect) {
    let k = rect.right_top() + vec2(-2.0, 2.0);
    let s = Stroke::new(2.0_f32, MARK);
    p.line_segment([k, k + vec2(-7.0, 0.0)], s);
    p.line_segment([k, k + vec2(0.0, 7.0)], s);
}

/// A texture of `size` scaled to fit inside `rect`, centred.
pub fn image_fit(p: &egui::Painter, tex: egui::TextureId, size: egui::Vec2, rect: Rect, tint: Color32) {
    let k = (rect.width() / size.x).min(rect.height() / size.y);
    p.image(tex, Rect::from_center_size(rect.center(), size * k), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
}

/// A frame for panels: black with a quiet edge.
pub fn panel_frame() -> egui::Frame {
    egui::Frame::new().fill(BLACK).inner_margin(egui::Margin::symmetric(12, 10))
}

