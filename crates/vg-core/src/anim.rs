//! Procedural motion: the squash-and-pop on a pose switch, idle bob and breathing,
//! and the scale-in/out for `[show]`/`[hide]`. Pure functions of time, so the GUI
//! preview and the exporter always agree frame for frame.

use crate::poses::Pose;
use crate::preset::AvatarLayout;
use crate::timeline::{Action, Timeline};
use serde::{Deserialize, Serialize};
use std::f32::consts::TAU;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Motion {
    /// Squash depth at the instant of a switch (0 = no pop).
    pub pop_amount: f32,
    /// Spring frequency of the pop, Hz.
    pub pop_freq: f32,
    /// Spring decay rate, 1/s (higher settles faster).
    pub pop_damping: f32,
    /// Idle vertical bob, as a fraction of the avatar's height (0 = still).
    pub bob_amount: f32,
    /// Seconds per bob / breath cycle.
    pub bob_period: f32,
    /// Idle vertical breathing stretch (fraction).
    pub breathe_amount: f32,
    /// Seconds for `[hide]` to shrink the avatar away.
    pub hide_time: f32,
}

impl Default for Motion {
    fn default() -> Self {
        Motion {
            pop_amount: 0.12,
            pop_freq: 6.0,
            pop_damping: 18.0,
            bob_amount: 0.006,
            bob_period: 2.6,
            breathe_amount: 0.006,
            hide_time: 0.15,
        }
    }
}

/// What to draw for the avatar at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpriteState {
    pub pose: usize,
    /// Multipliers on width and height, around the pose's anchor.
    pub scale: (f32, f32),
    /// Extra offset in units of the avatar's display height.
    pub offset: (f32, f32),
}

/// Damped spring from 0 toward 1: `1 - e^(-dτ) cos(ωτ)`; starts at 0, overshoots, settles.
fn spring(tau: f32, freq: f32, damping: f32) -> f32 {
    1.0 - (-damping * tau).exp() * (TAU * freq * tau).cos()
}

/// The avatar at time `t`, or `None` while it is hidden.
pub fn avatar_at(tl: &Timeline, motion: &Motion, t: f64) -> Option<SpriteState> {
    let mut pose = tl.initial_pose;
    let mut switched_at: Option<f64> = None;
    let mut shown_at: Option<f64> = None;
    let mut hidden_at: Option<f64> = None;
    for ev in tl.events.iter().take_while(|e| e.t <= t) {
        match ev.action {
            Action::Pose(p) => {
                // The opening pose at t=0 shouldn't pop.
                if ev.t > 0.0 || p != pose {
                    switched_at = Some(ev.t);
                }
                pose = p;
                if hidden_at.is_some() {
                    // A pose tag while hidden brings the avatar back.
                    hidden_at = None;
                    shown_at = Some(ev.t);
                }
            }
            Action::Hide => {
                if hidden_at.is_none() {
                    hidden_at = Some(ev.t);
                }
            }
            Action::Show => {
                if hidden_at.take().is_some() {
                    shown_at = Some(ev.t);
                }
            }
        }
    }

    let mut uniform = 1.0f32;
    if let Some(h) = hidden_at {
        let p = ((t - h) as f32 / motion.hide_time.max(1e-3)).min(1.0);
        if p >= 1.0 {
            return None;
        }
        uniform = 1.0 - p * p;
    }
    if let Some(s) = shown_at {
        let tau = (t - s) as f32;
        if tau < 1.0 {
            uniform *= spring(tau, motion.pop_freq, motion.pop_damping).max(0.0);
        }
    }

    // Squash on a switch: start short and wide, spring back through tall and thin.
    let mut squash = 0.0;
    if let Some(s) = switched_at {
        let tau = (t - s) as f32;
        if tau < 1.0 {
            squash = -motion.pop_amount * (-motion.pop_damping * tau).exp() * (TAU * motion.pop_freq * tau).cos();
        }
    }
    let phase = TAU * (t as f32) / motion.bob_period.max(0.1);
    let breathe = motion.breathe_amount * phase.sin();
    let sy = uniform * (1.0 + squash + breathe);
    let sx = uniform * (1.0 - 0.6 * squash);
    let bob = motion.bob_amount * (phase + 0.8).sin();
    Some(SpriteState { pose, scale: (sx, sy), offset: (0.0, bob) })
}

/// Canvas-pixel corners (top-left, top-right, bottom-left, bottom-right) of the
/// pose image for this state, with texture u flipped when the layout mirrors it.
pub fn sprite_quad(layout: &AvatarLayout, canvas: (u32, u32), pose: &Pose, st: &SpriteState) -> [[f32; 2]; 4] {
    let (cw, ch) = (canvas.0 as f32, canvas.1 as f32);
    let (iw, ih) = (pose.size.0.max(1) as f32, pose.size.1.max(1) as f32);
    let spot = layout.spot(&pose.tag);
    let dh = spot.height * ch * pose.scale;
    let dw = dh * iw / ih;
    let flip = if layout.flip { -1.0 } else { 1.0 };
    let ax = spot.x * cw + (pose.offset.0 * flip + st.offset.0) * dh;
    let ay = spot.y * ch + (pose.offset.1 + st.offset.1) * dh;
    let corner = |u: f32, v: f32| {
        let lx = (u - pose.anchor.0) * dw * st.scale.0 * flip;
        let ly = (v - pose.anchor.1) * dh * st.scale.1;
        [ax + lx, ay + ly]
    };
    [corner(0.0, 0.0), corner(1.0, 0.0), corner(0.0, 1.0), corner(1.0, 1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cues::CueSheet;
    use crate::poses::{Pose, PoseSet};
    use std::path::PathBuf;

    fn tl(src: &str) -> Timeline {
        let mk = |t: &str| Pose { tag: t.into(), file: PathBuf::from("x.png"), size: (100, 200), anchor: (0.5, 1.0), scale: 1.0, ..Default::default() };
        let poses = PoseSet { poses: vec![mk("a"), mk("b")], base: PathBuf::new() };
        Timeline::build(&CueSheet::parse(src).unwrap(), &poses, Some(10.0)).unwrap()
    }

    fn still() -> Motion {
        Motion { bob_amount: 0.0, breathe_amount: 0.0, ..Motion::default() }
    }

    #[test]
    fn spring_shape() {
        assert_eq!(spring(0.0, 6.0, 18.0), 0.0);
        let peak = (1..60).map(|i| spring(i as f32 / 200.0, 6.0, 18.0)).fold(0.0, f32::max);
        assert!(peak > 1.02 && peak < 1.3, "overshoot {peak}");
        assert!((spring(0.5, 6.0, 18.0) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn pose_switch_pops_then_settles() {
        let tl = tl("[a@0] [b@2]");
        let m = still();
        let before = avatar_at(&tl, &m, 1.9).unwrap();
        assert_eq!(before.pose, 0);
        assert_eq!(before.scale, (1.0, 1.0), "opening pose must not pop");
        let at = avatar_at(&tl, &m, 2.0).unwrap();
        assert_eq!(at.pose, 1);
        assert!((at.scale.1 - (1.0 - m.pop_amount)).abs() < 1e-5);
        assert!(at.scale.0 > 1.0);
        let later = avatar_at(&tl, &m, 2.6).unwrap();
        assert!((later.scale.1 - 1.0).abs() < 1e-3);
    }

    #[test]
    fn hide_and_show() {
        let tl = tl("[a@0] [hide@1] [show@3]");
        let m = still();
        let mid = avatar_at(&tl, &m, 1.0 + m.hide_time as f64 / 2.0).unwrap();
        assert!(mid.scale.0 < 1.0 && mid.scale.0 > 0.0);
        assert!(avatar_at(&tl, &m, 2.0).is_none());
        let back = avatar_at(&tl, &m, 3.0).unwrap();
        assert_eq!(back.scale.0, 0.0);
        assert!((avatar_at(&tl, &m, 4.5).unwrap().scale.0 - 1.0).abs() < 1e-3);
    }

    #[test]
    fn quad_places_anchor_on_layout_point() {
        let mut layout = AvatarLayout { x: 0.5, y: 1.0, height: 0.5, ..Default::default() };
        let pose = Pose { tag: "a".into(), size: (100, 200), anchor: (0.5, 1.0), scale: 1.0, ..Default::default() };
        let st = SpriteState { pose: 0, scale: (1.0, 1.0), offset: (0.0, 0.0) };
        let q = sprite_quad(&layout, (1000, 1000), &pose, &st);
        assert_eq!(q[0], [375.0, 500.0]);
        assert_eq!(q[3], [625.0, 1000.0]);
        let flipped = sprite_quad(&AvatarLayout { flip: true, ..layout.clone() }, (1000, 1000), &pose, &st);
        assert_eq!(flipped[0], [625.0, 500.0]);
        // A pose with its own spot moves alone.
        *layout.spot_mut("a") = crate::preset::Spot { x: 0.25, y: 0.5, height: 0.25 };
        let own = sprite_quad(&layout, (1000, 1000), &pose, &st);
        assert_eq!(own[3], [312.5, 500.0]);
        let other = Pose { tag: "b".into(), ..pose };
        assert_eq!(sprite_quad(&layout, (1000, 1000), &other, &st), q);
    }
}
