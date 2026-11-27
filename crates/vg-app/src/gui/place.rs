//! Where floating windows open: each has its own home spot (so they're easy to
//! tell apart), and a window that would land on another open one is moved to
//! the nearest free space instead. Only when the screen is full do they overlap.
//! A window the user drags keeps that spot for the rest of the session.

use eframe::egui::{self, Align2, Id, Pos2, Rect, Vec2};
use std::collections::HashMap;

#[derive(Default)]
pub struct Placer {
    wins: HashMap<&'static str, Win>,
}

#[derive(Default)]
struct Win {
    /// The last pass the window was shown in.
    seen: Option<u64>,
    /// Where it was put (top-left), to notice when the user moves it.
    placed: Option<Pos2>,
    /// Placed with a guessed size; place again once the real size is known.
    guessed: bool,
    /// Where the user dragged it.
    moved_to: Option<Pos2>,
}

fn id(key: &str) -> Id {
    Id::new(("window", key))
}

/// The usable area: below the header, inside a small margin.
fn area(ctx: &egui::Context) -> Rect {
    let r = ctx.content_rect();
    Rect::from_min_max(r.min + egui::vec2(12.0, 50.0), r.max - egui::vec2(12.0, 12.0))
}

impl Placer {
    /// Call each frame a window is open, before showing it through [`at`] with
    /// the same key. Returns the position (top-left) to force on the frames
    /// it's being placed.
    pub fn place(&mut self, ctx: &egui::Context, key: &'static str, home: Align2, guess: Vec2) -> Option<Pos2> {
        let pass = ctx.cumulative_pass_nr();
        let known = ctx.memory(|m| m.area_rect(id(key)));
        let w = self.wins.entry(key).or_default();
        let opening = w.seen.is_none_or(|s| s + 1 < pass);
        w.seen = Some(pass);
        if !opening {
            let placed = w.placed?;
            let rect = known?;
            if w.guessed {
                // Its first frame is laid out invisibly; now the size is real.
                w.guessed = false;
            } else {
                if (rect.min - placed).length() > 1.0 {
                    w.moved_to = Some(rect.min);
                    w.placed = Some(rect.min);
                }
                return None;
            }
        }
        let size = known.map_or(guess, |r| r.size());
        let guessed = known.is_none();
        let a = area(ctx);
        let want = self.wins[key].moved_to.unwrap_or_else(|| home.align_size_within_rect(size, a).min);
        // Everything else on screen now (shown this pass or the last).
        let others: Vec<Rect> = self
            .wins
            .iter()
            .filter(|(k, o)| **k != key && o.seen.is_some_and(|s| s + 1 >= pass))
            .filter_map(|(k, _)| ctx.memory(|m| m.area_rect(id(k))))
            .collect();
        let pos = free_spot(a, size, want, &others);
        let w = self.wins.get_mut(key).unwrap();
        w.placed = Some(pos);
        w.guessed = guessed;
        Some(pos)
    }
}

/// Force the window to `pos` when it's being placed.
pub fn at<'a>(w: egui::Window<'a>, key: &'static str, pos: Option<Pos2>) -> egui::Window<'a> {
    let w = w.id(id(key));
    match pos {
        Some(p) => w.current_pos(p),
        None => w,
    }
}

/// The free spot nearest `want` for a window of `size`, or `want` itself (a
/// little offset from whatever it covers) when nothing is free.
fn free_spot(area: Rect, size: Vec2, want: Pos2, others: &[Rect]) -> Pos2 {
    let gap = 8.0;
    let fits = |p: Pos2| {
        let r = Rect::from_min_size(p, size);
        !others.iter().any(|o| o.expand(gap - 0.5).intersects(r))
    };
    let clamp = |p: Pos2| Pos2::new(p.x.clamp(area.left(), (area.right() - size.x).max(area.left())), p.y.clamp(area.top(), (area.bottom() - size.y).max(area.top())));
    let want = clamp(want);
    if fits(want) {
        return want;
    }
    // Candidates: beside each open window, and a coarse grid; nearest first.
    let mut cands: Vec<Pos2> = vec![];
    for o in others {
        cands.push(Pos2::new(o.right() + gap, want.y));
        cands.push(Pos2::new(o.left() - gap - size.x, want.y));
        cands.push(Pos2::new(want.x, o.bottom() + gap));
        cands.push(Pos2::new(want.x, o.top() - gap - size.y));
        cands.push(Pos2::new(o.right() + gap, o.top()));
        cands.push(Pos2::new(o.left() - gap - size.x, o.top()));
    }
    let step = 24.0;
    let (nx, ny) = (((area.width() - size.x) / step).max(0.0) as usize, ((area.height() - size.y) / step).max(0.0) as usize);
    for i in 0..=nx {
        for j in 0..=ny {
            cands.push(Pos2::new(area.left() + i as f32 * step, area.top() + j as f32 * step));
        }
    }
    let inside = |p: &Pos2| p.x >= area.left() - 0.5 && p.y >= area.top() - 0.5 && p.x + size.x <= area.right() + 0.5 && p.y + size.y <= area.bottom() + 0.5;
    cands
        .into_iter()
        .filter(|p| inside(p) && fits(*p))
        .min_by(|a, b| a.distance_sq(want).total_cmp(&b.distance_sq(want)))
        .unwrap_or_else(|| clamp(want + egui::vec2(28.0, 28.0) * others.iter().filter(|o| o.contains(want)).count() as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_off_an_open_window_and_overlaps_only_when_full() {
        let area = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));
        let size = Vec2::new(300.0, 300.0);
        let other = Rect::from_min_size(Pos2::new(700.0, 0.0), Vec2::new(300.0, 400.0));
        // Wanted the top-right corner, which is taken: it goes just left of it.
        let p = free_spot(area, size, Pos2::new(700.0, 0.0), &[other]);
        assert!(!Rect::from_min_size(p, size).intersects(other));
        assert_eq!(p, Pos2::new(392.0, 0.0));
        // No room anywhere: it still opens, where it wanted to.
        let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 800.0));
        let p = free_spot(area, size, Pos2::new(100.0, 100.0), &[full]);
        assert!(area.contains_rect(Rect::from_min_size(p, size)));
    }
}
