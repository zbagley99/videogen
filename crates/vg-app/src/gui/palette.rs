//! The pose palette: thumbnails to click (inserts a tag) and a small inspector
//! for per-pose scale / offset / anchor / aliases.

use super::util::{file_name, plural, thumbnail};
use super::{VgApp, theme};
use eframe::egui;
use vg_core::PoseSet;

#[derive(Default)]
pub struct Palette {
    thumbs: Vec<Option<egui::TextureHandle>>,
    pub(super) selected: Option<usize>,
    alias_edit: Option<(usize, String)>,
    /// "Good for" edited one per line: (pose, text).
    uses_edit: Option<(usize, String)>,
    /// Height of the inspector under the thumbnails (dragged at the splitter).
    inspector_h: Option<f32>,
    /// Asking before deleting a pose: (pose, also trash its image).
    delete: Option<(usize, bool)>,
}

impl Palette {
    pub fn load_thumbnails(&mut self, ctx: &egui::Context, poses: &PoseSet) {
        let paths: Vec<_> = (0..poses.poses.len()).map(|i| poses.path_of(i)).collect();
        let images: Vec<Option<egui::ColorImage>> = std::thread::scope(|s| {
            let handles: Vec<_> = paths
                .iter()
                .map(|p| {
                    s.spawn(move || image::open(p).ok().map(|img| thumbnail(&img)))
                })
                .collect();
            handles.into_iter().map(|h| h.join().ok().flatten()).collect()
        });
        self.thumbs = images
            .into_iter()
            .enumerate()
            .map(|(i, img)| img.map(|img| ctx.load_texture(format!("pose-thumb-{i}"), img, egui::TextureOptions::LINEAR)))
            .collect();
        if self.selected.is_some_and(|s| s >= poses.poses.len()) {
            self.selected = None;
        }
        self.alias_edit = None;
        self.uses_edit = None;
    }
}

pub fn show(app: &mut VgApp, ui: &mut egui::Ui) {
    confirm_delete(app, &ui.ctx().clone());
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::tool_button(ui, "Suggest", None, app.assist.open).clicked() {
                app.assist.open = !app.assist.open;
            }
            if theme::tool_button(ui, "Live", None, app.prefs.live_tag)
                .on_hover_text("Live tagging · clicking a pose (or pressing 1–9) adds [pose@time] on the line at the playhead instead of at the text cursor. Play the narration and tag as you listen.")
                .clicked()
            {
                app.prefs.live_tag = !app.prefs.live_tag;
            }
        });
    });
    if app.poses.poses.is_empty() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("No pose library loaded.").color(theme::TEXT2));
        ui.label(egui::RichText::new("Point it at a folder of transparent PNGs, one per pose. File names become tags.").color(theme::MUTED));
        ui.add_space(6.0);
        if theme::tool_button(ui, "Create from art folder", None, false).clicked() {
            app.create_pose_library();
        }
        return;
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(if app.prefs.live_tag { "Click tags the line at the playhead" } else { "Click inserts at the text cursor · right-click adjusts" })
                    .size(11.0)
                    .color(theme::MUTED),
            )
            .wrap(),
        );
    });
    ui.horizontal(|ui| {
        // Structured data about every pose, for an LLM chat or notes.
        if theme::tool_button(ui, "Copy list", None, false).clicked() {
            ui.ctx().copy_text(app.poses.summary_json());
            app.info(format!("copied {} poses as JSON: tag, name to write, aliases, description, good for, key, image", app.poses.poses.len()));
        }
        theme::note(ui, "all poses as JSON");
    });
    ui.add_space(4.0);

    // Which pose is showing right now, to mark it.
    let current = app
        .parsed
        .timeline
        .as_ref()
        .and_then(|tl| vg_core::avatar_at(tl, &app.project.preset.motion, app.time))
        .map(|s| s.pose);

    // The inspector shares the column with the thumbnails; a splitter between
    // them sets its height, and it scrolls on its own.
    const SPLIT: f32 = 9.0;
    let avail = ui.available_height();
    let max_h = (avail - 120.0).max(140.0);
    let inspector_h = if app.palette.selected.is_some() { app.palette.inspector_h.unwrap_or(avail * 0.5).clamp(140.0, max_h) } else { 0.0 };
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt("pose-grid").max_height(avail - inspector_h - if inspector_h > 0.0 { SPLIT } else { 0.0 }).auto_shrink([false, false]).show(ui, |ui| {
        let gap = 6.0;
        let cols = ((ui.available_width() + gap) / (110.0 + gap)).floor().max(1.0) as usize;
        let cell_w = ((ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32).floor();
        let cell = egui::vec2(cell_w, cell_w + 20.0);
        for (r, row) in app.poses.poses.chunks(cols).enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (j, pose) in row.iter().enumerate() {
                    let i = r * cols + j;
                    let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::click());
                    let p = ui.painter();
                    let selected = app.palette.selected == Some(i);
                    let showing = current == Some(i);
                    let hovered = resp.hovered();
                    if hovered {
                        p.rect_filled(rect, 1.0, theme::HOVER);
                    }
                    let edge = if showing { theme::MARK } else if hovered || selected { theme::FOCUS } else { theme::RULE };
                    p.rect_stroke(rect, 1.0, egui::Stroke::new(1.0_f32, edge), egui::StrokeKind::Inside);
                    if showing {
                        theme::marked_corner(p, rect);
                    }
                    let img_rect = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 6.0), egui::vec2(cell_w - 12.0, cell_w - 12.0));
                    if let Some(Some(tex)) = app.palette.thumbs.get(i) {
                        let tint = if hovered || showing || selected { egui::Color32::WHITE } else { egui::Color32::from_gray(215) };
                        theme::image_fit(p, tex.id(), tex.size_vec2(), img_rect, tint);
                    }
                    let mut job = egui::text::LayoutJob::default();
                    job.append(
                        &pose.short_name().to_uppercase(),
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(10.0),
                            color: if showing { theme::MARK } else if hovered { theme::WHITE } else { theme::TEXT2 },
                            extra_letter_spacing: 0.9,
                            ..Default::default()
                        },
                    );
                    let g = p.layout_job(job);
                    p.galley(egui::pos2(rect.center().x - g.size().x / 2.0, rect.bottom() - 16.0), g, theme::TEXT2);
                    if i < 9 {
                        let kr = egui::Rect::from_min_size(rect.min + egui::vec2(5.0, 5.0), egui::vec2(14.0, 14.0));
                        p.rect_filled(kr, 1.0, theme::BLACK);
                        p.rect_stroke(kr, 1.0, egui::Stroke::new(1.0_f32, theme::RULE), egui::StrokeKind::Inside);
                        p.text(kr.center(), egui::Align2::CENTER_CENTER, format!("{}", i + 1), egui::FontId::monospace(9.5), theme::MUTED);
                    }
                    let tip = if pose.aliases.is_empty() { format!("[{}]", pose.tag) } else { format!("[{}] · also {}", pose.tag, pose.aliases.join(", ")) };
                    let resp = resp.on_hover_text(format!("{tip}\nClick to tag · right-click to adjust")).on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.clicked() {
                        clicked = Some(i);
                    }
                    if resp.secondary_clicked() {
                        app.palette.selected = if selected { None } else { Some(i) };
                    }
                }
            });
            ui.add_space(gap - ui.spacing().item_spacing.y);
        }
    });
    if let Some(i) = clicked {
        let ctx = ui.ctx().clone();
        app.insert_pose_tag(&ctx, i);
    }

    if let Some(i) = app.palette.selected {
        // Splitter: a rule that brightens under the pointer; drag to resize.
        let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), SPLIT), egui::Sense::drag());
        let hot = resp.hovered() || resp.dragged();
        ui.painter().line_segment([r.left_center(), r.right_center()], egui::Stroke::new(1.0_f32, if hot { theme::FOCUS } else { theme::RULE }));
        if hot {
            ui.painter().rect_filled(egui::Rect::from_center_size(r.center(), egui::vec2(24.0, 3.0)), 0.0, if resp.dragged() { theme::WHITE } else { theme::FOCUS });
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resp.dragged() {
            app.palette.inspector_h = Some((inspector_h - resp.drag_delta().y).clamp(140.0, max_h));
        }
        egui::ScrollArea::vertical().id_salt("pose-inspector").max_height(inspector_h).auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            inspector(app, ui, i);
        });
    }
}

fn inspector(app: &mut VgApp, ui: &mut egui::Ui, i: usize) {
    let mut changed = false;
    ui.horizontal(|ui| {
        theme::panel_title(ui, "Adjust", "");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::tool_button(ui, "Close", None, false).clicked() {
                app.palette.selected = None;
            }
            if theme::danger_button(ui, "Delete").clicked() {
                app.palette.delete = Some((i, false));
            }
        });
    });
    ui.add(egui::Label::new(egui::RichText::new(format!("[{}]", app.poses.poses[i].tag)).monospace().size(11.5).color(theme::MARK)).wrap());
    // Aliases are edited as a comma list and applied when the field loses focus.
    let edit = app.palette.alias_edit.get_or_insert_with(|| (i, app.poses.poses[i].aliases.join(", ")));
    if edit.0 != i {
        *edit = (i, app.poses.poses[i].aliases.join(", "));
    }
    let resp = ui.horizontal(|ui| {
        ui.label(theme::caption("Aliases"));
        ui.text_edit_singleline(&mut edit.1)
    });
    if resp.inner.lost_focus() {
        let new: Vec<String> = edit.1.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
        let old = std::mem::replace(&mut app.poses.poses[i].aliases, new);
        if let Err(e) = app.poses.check() {
            app.poses.poses[i].aliases = old;
            app.error(format!("{e}"));
        } else {
            changed = app.poses.poses[i].aliases != old;
        }
    }
    changed |= description_fields(app, ui, i);
    // Where it sits on the canvas (the project's, per format).
    let tag = app.poses.poses[i].tag.clone();
    let layout = &mut app.project.preset.avatar;
    ui.horizontal(|ui| {
        ui.label(theme::caption("Canvas spot"));
        match layout.poses.get(&tag).copied() {
            Some(s) => {
                ui.label(egui::RichText::new(format!("its own · x {:.2} y {:.2} size {:.2}", s.x, s.y, s.height)).size(11.0).color(theme::TEXT2));
                if theme::tool_button(ui, "Use default", None, false).clicked() {
                    layout.poses.remove(&tag);
                }
            }
            None => {
                theme::note(ui, "default · drag it on the canvas to place it");
            }
        }
    });
    let pose = &mut app.poses.poses[i];
    egui::Grid::new("pose-inspector").num_columns(2).show(ui, |ui| {
        ui.label(theme::caption("Scale"));
        changed |= ui.add(egui::Slider::new(&mut pose.scale, 0.3..=2.0).fixed_decimals(2)).changed();
        ui.end_row();
        ui.label(theme::caption("Offset x"));
        changed |= ui.add(egui::Slider::new(&mut pose.offset.0, -0.5..=0.5).fixed_decimals(3)).changed();
        ui.end_row();
        ui.label(theme::caption("Offset y"));
        changed |= ui.add(egui::Slider::new(&mut pose.offset.1, -0.5..=0.5).fixed_decimals(3)).changed();
        ui.end_row();
        ui.label(theme::caption("Anchor")).on_hover_text("The point of the image (0–1) pinned to the layout position; normally the bottom-centre of the body.");
        ui.horizontal(|ui| {
            changed |= ui.add(egui::DragValue::new(&mut pose.anchor.0).range(0.0..=1.0).speed(0.002).fixed_decimals(3)).changed();
            changed |= ui.add(egui::DragValue::new(&mut pose.anchor.1).range(0.0..=1.0).speed(0.002).fixed_decimals(3)).changed();
        });
        ui.end_row();
    });
    let can_save = app.poses_dirty || changed;
    let (reset, save) = ui
        .horizontal(|ui| (theme::tool_button(ui, "Reset", None, false).clicked(), theme::tool_button_enabled(ui, "Save library", None, false, can_save).clicked()))
        .inner;
    if reset {
        pose.scale = 1.0;
        pose.offset = (0.0, 0.0);
        changed = true;
    }
    if save {
        app.poses_dirty = true;
        if let Err(e) = app.save_poses() {
            app.error(format!("{e:#}"));
        }
        changed = false;
    }
    if changed {
        app.poses_dirty = true;
        app.reparse();
    }
}

/// Ask before deleting a pose, then take it out of the library (and, if
/// ticked, its image to the trash).
fn confirm_delete(app: &mut VgApp, ctx: &egui::Context) {
    let Some((i, trash_image)) = app.palette.delete else { return };
    let Some(pose) = app.poses.poses.get(i) else {
        app.palette.delete = None;
        return;
    };
    let tag = pose.tag.clone();
    let image = app.poses.path_of(i);
    let image_shared = (0..app.poses.poses.len()).any(|j| j != i && app.poses.path_of(j) == image);
    let uses = app.parsed.timeline.as_ref().map(|tl| tl.events.iter().filter(|e| matches!(e.action, vg_core::Action::Pose(p) if p == i)).count()).unwrap_or(0);
    let mut keep_trash = trash_image;
    let mut choice = None;
    egui::Modal::new(egui::Id::new("pose-delete")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.label(theme::caption("Delete pose").color(theme::WHITE));
        ui.add_space(4.0);
        ui.label(egui::RichText::new(format!("[{tag}]")).monospace().color(theme::MARK));
        ui.add(egui::Label::new(egui::RichText::new("It's removed from the pose library, which is saved straight away (with any other unsaved pose edits). Other projects that use this library lose it too.").color(theme::TEXT2)).wrap());
        if uses > 0 {
            ui.add_space(4.0);
            ui.add(egui::Label::new(egui::RichText::new(format!("The open script uses it {uses} time{}; those tags will show as unknown until you change them.", plural(uses))).color(theme::WHITE)).wrap());
        }
        ui.add_space(6.0);
        let name = file_name(&image);
        if image_shared {
            theme::note(ui, format!("{name} stays: another pose uses it."));
        } else {
            ui.checkbox(&mut keep_trash, egui::RichText::new(format!("Also move {name} to the trash")).size(12.0));
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if theme::danger_button(ui, "Delete pose").clicked() {
                choice = Some(true);
            }
            if theme::tool_button(ui, "Cancel", None, false).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                choice = Some(false);
            }
        });
    });
    app.palette.delete = Some((i, keep_trash));
    match choice {
        Some(true) => {
            app.palette.delete = None;
            let Some(path) = app.poses_path.clone() else { return app.error("the pose library has no file to save to") };
            app.poses.poses.remove(i);
            // The last good timeline (kept while the script has an error, as it
            // will if it uses this pose) numbers poses the old way: renumber it.
            if let Some(tl) = &mut app.parsed.timeline {
                tl.events.retain(|e| !matches!(e.action, vg_core::Action::Pose(p) if p == i));
                for e in &mut tl.events {
                    if let vg_core::Action::Pose(p) = &mut e.action
                        && *p > i {
                            *p -= 1;
                        }
                }
            }
            if let Err(e) = app.poses.save(&path) {
                return app.error(format!("{e:#}"));
            }
            let trashed = keep_trash && !image_shared && trash::delete(&image).is_ok();
            app.project.preset.avatar.poses.remove(&tag);
            app.palette.selected = None;
            app.palette.alias_edit = None;
            app.palette.uses_edit = None;
            if let Err(e) = app.load_poses(ctx, &path) {
                return app.error(format!("{e:#}"));
            }
            app.info(format!("deleted [{tag}]{}", if trashed { " and moved its image to the trash" } else { "" }));
        }
        Some(false) => app.palette.delete = None,
        None => {}
    }
}

/// What the pose looks like and when it fits (read by the LLM that suggests
/// poses).
fn description_fields(app: &mut VgApp, ui: &mut egui::Ui, i: usize) -> bool {
    let mut changed = false;
    ui.label(theme::caption("Looks like"));
    changed |= ui.add(egui::TextEdit::multiline(&mut app.poses.poses[i].description).desired_rows(2).desired_width(f32::INFINITY).hint_text("e.g. hand over the face, exasperated")).changed();
    ui.label(theme::caption("Good for (one per line)"));
    let edit = app.palette.uses_edit.get_or_insert_with(|| (i, app.poses.poses[i].use_when.join("\n")));
    if edit.0 != i {
        *edit = (i, app.poses.poses[i].use_when.join("\n"));
    }
    let r = ui.add(egui::TextEdit::multiline(&mut edit.1).desired_rows(2).desired_width(f32::INFINITY).hint_text("a mistake is revealed"));
    if r.changed() {
        app.poses.poses[i].use_when = edit.1.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        changed = true;
    }
    ui.add_space(4.0);
    changed
}
