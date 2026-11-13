//! A visitor that emerges from snow, explores, then burrows or walks away.
use snowhush_core::geometry::V2;

pub const SIZE: f32 = 100.0;
/// Snow area in a 30-point patch, enough to make a visible drift rather than a dusting.
pub const BURROW_SNOW: f32 = 60.0;
pub const SUPPORT_OFFSETS: [f32; 5] = [-34.0, -17.0, 0.0, 17.0, 34.0];
const VISIT: f32 = 18.0;
const TRANSITION: f32 = 2.0;
const RISE: f32 = 0.35;
const NOTICE: f32 = 130.0;
const STARTLE: f32 = 62.0;

/// A mound needs continuous snow under its full width and a reasonably even surface.
pub fn suitable_drift(area: f32, samples: [(f32, f32); 5]) -> bool {
    if !area.is_finite() || area < BURROW_SNOW || samples.iter().any(|(height, snow)| !height.is_finite() || !snow.is_finite() || *snow < 2.0) {
        return false;
    }
    let (low, high) = samples.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), (height, _)| (low.min(*height), high.max(*height)));
    high - low <= 10.0
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Emerging { elapsed: f32 },
    Settling { elapsed: f32 },
    Visiting,
    Fleeing { elapsed: f32 },
    Preparing { elapsed: f32 },
    Burrowing { elapsed: f32 },
    Leaving,
}

pub struct Fox {
    pub x: f32,
    age: f32,
    direction: f32,
    walking_time: f32,
    walking: bool,
    phase: Phase,
}

impl Fox {
    pub fn new(x: f32, right: bool) -> Self {
        Self { x, age: 0.0, direction: if right { 1.0 } else { -1.0 }, walking_time: 0.0, walking: false, phase: Phase::Emerging { elapsed: 0.0 } }
    }

    fn leave_or_burrow(&mut self, can_burrow: bool, width: f32) {
        self.walking = false;
        if can_burrow {
            self.phase = Phase::Preparing { elapsed: 0.0 };
        } else {
            self.direction = if self.x < width * 0.5 { -1.0 } else { 1.0 };
            self.phase = Phase::Leaving;
        }
    }

    /// Normal steps stay on snow. If the drift is lost, leaving may cross bare ground and exits completely offscreen.
    pub fn advance(&mut self, dt: f32, cursor: V2, ground: f32, width: f32, can_burrow: bool, can_step: impl Fn(f32) -> bool) -> bool {
        if !dt.is_finite() || dt <= 0.0 {
            return false;
        }
        let dt = dt.min(0.05);
        match self.phase {
            Phase::Emerging { elapsed } => {
                if !can_burrow {
                    // A disappearing support cannot carry a drawn mound or a partly buried fox.
                    self.leave_or_burrow(false, width);
                    return false;
                }
                let elapsed = elapsed + dt;
                if elapsed >= TRANSITION {
                    self.phase = Phase::Settling { elapsed: 0.0 };
                } else {
                    self.phase = Phase::Emerging { elapsed };
                }
            }
            Phase::Settling { elapsed } => {
                if !can_burrow {
                    self.leave_or_burrow(false, width);
                    return false;
                }
                let elapsed = elapsed + dt;
                if elapsed >= RISE {
                    self.phase = Phase::Visiting;
                } else {
                    self.phase = Phase::Settling { elapsed };
                }
            }
            Phase::Preparing { elapsed } => {
                if !can_burrow {
                    self.leave_or_burrow(false, width);
                } else if elapsed + dt >= RISE {
                    self.phase = Phase::Burrowing { elapsed: 0.0 };
                } else {
                    self.phase = Phase::Preparing { elapsed: elapsed + dt };
                }
            }
            Phase::Burrowing { elapsed } => {
                if !can_burrow {
                    self.leave_or_burrow(false, width);
                } else if elapsed + dt >= TRANSITION {
                    return true;
                } else {
                    self.phase = Phase::Burrowing { elapsed: elapsed + dt };
                }
            }
            Phase::Visiting => {
                self.age += dt;
                let distance = (cursor - V2::new(self.x, ground + SIZE * 0.25)).length();
                if distance < STARTLE {
                    self.direction = if cursor.x < self.x { 1.0 } else { -1.0 };
                    self.phase = Phase::Fleeing { elapsed: 0.0 };
                } else if self.age >= VISIT {
                    self.leave_or_burrow(can_burrow, width);
                } else {
                    let noticing = distance < NOTICE;
                    if noticing {
                        self.direction = if cursor.x < self.x { -1.0 } else { 1.0 };
                    }
                    self.walking = !noticing && self.age % 5.0 < 2.5;
                    if self.walking {
                        let next = self.x + self.direction * 22.0 * dt;
                        if can_step(next) {
                            self.x = next;
                            self.walking_time += 7.0 * dt;
                        } else {
                            self.walking = false;
                            self.direction = -self.direction;
                        }
                    }
                }
            }
            Phase::Fleeing { elapsed } => {
                let next = self.x + self.direction * 60.0 * dt;
                if elapsed >= 1.5 || !can_step(next) {
                    self.leave_or_burrow(can_burrow, width);
                } else {
                    self.x = next;
                    self.walking = true;
                    self.walking_time += 11.0 * dt;
                    self.phase = Phase::Fleeing { elapsed: elapsed + dt };
                }
            }
            Phase::Leaving => {
                self.walking = true;
                self.x += self.direction * 70.0 * dt;
                self.walking_time += 11.0 * dt;
                return self.x + SIZE * 0.5 < 0.0 || self.x - SIZE * 0.5 > width;
            }
        }
        false
    }

    /// Atlas frame and how far the temporary mound is below the snow surface, or None while walking.
    pub fn burrow_frame(&self) -> Option<(f32, f32)> {
        let t = match self.phase {
            Phase::Emerging { elapsed, .. } => elapsed,
            Phase::Burrowing { elapsed } => TRANSITION - elapsed,
            _ => return None,
        }
        .clamp(0.0, TRANSITION);
        let frame = (((t - RISE).max(0.0) / (TRANSITION - RISE) * 4.0).floor() as u32).min(3) as f32;
        let below = 16.0 * (1.0 - t / RISE).clamp(0.0, 1.0);
        Some((frame, below))
    }

    /// Keep the mound beneath the standing fox while it rises or settles at the transition.
    pub fn bank_below(&self) -> Option<f32> {
        match self.phase {
            Phase::Preparing { elapsed } => Some(16.0 * (1.0 - elapsed / RISE).clamp(0.0, 1.0)),
            Phase::Settling { elapsed, .. } => Some(16.0 * (elapsed / RISE).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    pub fn frame(&self) -> f32 {
        if self.walking { (self.walking_time as u32 % 4) as f32 } else { 0.0 }
    }
    pub fn facing_right(&self) -> bool {
        self.direction > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FAR: V2 = V2 { x: -1000.0, y: -1000.0 };
    fn step(fox: &mut Fox, cursor: V2, snow: bool) -> bool {
        fox.advance(0.05, cursor, 0.0, 800.0, snow, |_| true)
    }

    #[test]
    fn emergence_and_burrow_play_in_opposite_order_without_fading() {
        let mut fox = Fox::new(400.0, true);
        let mut seen = [false; 4];
        while let Some((frame, _)) = fox.burrow_frame() {
            seen[frame as usize] = true;
            assert!(!step(&mut fox, FAR, true));
        }
        assert!(seen.into_iter().all(|v| v));
        while fox.phase == Phase::Visiting {
            assert!(!step(&mut fox, FAR, true));
        }
        let mut frames = Vec::new();
        loop {
            if let Some((frame, _)) = fox.burrow_frame() {
                frames.push(frame as u32);
            }
            if step(&mut fox, FAR, true) {
                break;
            }
        }
        assert!(frames.windows(2).all(|p| p[0] >= p[1]));
        assert_eq!(frames.first(), Some(&3));
        assert_eq!(frames.last(), Some(&0));
    }

    #[test]
    fn snow_gone_at_departure_means_walking_completely_offscreen() {
        let mut fox = Fox::new(400.0, true);
        while fox.phase != Phase::Visiting {
            step(&mut fox, FAR, true);
        }
        while fox.phase != Phase::Leaving {
            assert!(!step(&mut fox, FAR, false));
        }
        let mut count = 0;
        while !step(&mut fox, FAR, false) {
            count += 1;
            assert!(count < 1000);
        }
        assert!(fox.x - SIZE * 0.5 > 800.0 || fox.x + SIZE * 0.5 < 0.0);
    }

    #[test]
    fn clearing_snow_during_burrow_removes_unsupported_mound_and_leaves() {
        let mut fox = Fox::new(400.0, true);
        while fox.phase != Phase::Visiting {
            step(&mut fox, FAR, true);
        }
        while !matches!(fox.phase, Phase::Burrowing { .. }) {
            step(&mut fox, FAR, true);
        }
        for _ in 0..20 {
            step(&mut fox, FAR, true);
        }
        assert!(!step(&mut fox, FAR, false));
        assert_eq!(fox.phase, Phase::Leaving);
        assert!(fox.burrow_frame().is_none());
        assert!(fox.bank_below().is_none());
    }

    #[test]
    fn clearing_snow_during_emergence_removes_the_mound_immediately() {
        let mut fox = Fox::new(400.0, true);
        for _ in 0..15 {
            assert!(!step(&mut fox, FAR, true));
        }
        assert!(fox.burrow_frame().is_some());
        assert!(!step(&mut fox, FAR, false));
        assert_eq!(fox.phase, Phase::Leaving);
        assert!(fox.burrow_frame().is_none());
    }

    #[test]
    fn burrow_requires_continuous_level_snow_under_full_width() {
        let mut samples = [(5.0, 8.0); 5];
        assert!(suitable_drift(60.0, samples));
        assert!(!suitable_drift(59.0, samples));
        samples[0].1 = 0.0;
        assert!(!suitable_drift(80.0, samples));
        samples[0] = (16.0, 8.0);
        assert!(!suitable_drift(80.0, samples));
    }

    #[test]
    fn cursor_makes_fox_watch_then_flee() {
        let mut fox = Fox::new(400.0, true);
        while fox.phase != Phase::Visiting {
            step(&mut fox, FAR, true);
        }
        let x = fox.x;
        step(&mut fox, V2::new(x - 100.0, 25.0), true);
        assert!(!fox.facing_right());
        assert_eq!(fox.x, x);
        step(&mut fox, V2::new(x - 10.0, 25.0), true);
        assert!(matches!(fox.phase, Phase::Fleeing { .. }));
        step(&mut fox, FAR, true);
        assert!(fox.x > x);
    }
}
