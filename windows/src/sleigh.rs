//! Sleigh eligibility and smooth motion in display-local points.

use crate::plow;

/// The caller supplies only monitors enabled in settings.
pub fn monitor_eligible(snow: bool, covered: bool, full_screen: bool) -> bool {
    snow && !covered && !full_screen
}

pub fn can_start(enabled: bool, flying: bool) -> bool {
    enabled && !flying
}

#[derive(Clone, Copy)]
pub struct Sleigh {
    pub x: f32,
    pub y: f32,
    pub direction: f32,
    pub age: f32,
    pub sprite_width: f32,
    width: f32,
    base_y: f32,
    amplitude: f32,
    phase: f32,
}

impl Sleigh {
    pub fn new(width: f32, height: f32, physical_width: i32, physical_height: i32, random: u64) -> Self {
        let unit = |shift: u32| ((random >> shift) & 0xffff_u64) as f32 / 65535.0;
        let direction = if random & 1 == 0 { 1.0 } else { -1.0 };
        let sprite_width = plow::sprite_width(physical_width, physical_height) * 2.5;
        // Include the slight rotation of the quad in the offscreen margin.
        let margin = sprite_width * 0.53;
        let x = if direction > 0.0 { -margin } else { width + margin };
        let base_y = height * (0.48 + 0.25 * unit(8));
        let amplitude = height.min(700.0) * (0.025 + 0.025 * unit(24));
        let phase = unit(40) * std::f32::consts::TAU;
        let y = base_y + amplitude * phase.sin();
        Self { x, y, direction, age: 0.0, sprite_width, width, base_y, amplitude, phase }
    }

    pub fn advance(&mut self, dt: f32) -> bool {
        self.age += dt;
        self.x += self.direction * plow::SPEED * 2.5 * dt;
        self.y = self.base_y + self.amplitude * (self.phase + self.age * 0.25).sin();
        let margin = self.sprite_width * 0.53;
        if self.direction > 0.0 { self.x - margin >= self.width } else { self.x + margin <= 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flight_requires_the_setting_and_no_other_flight() {
        assert!(!can_start(false, false));
        assert!(!can_start(false, true));
        assert!(!can_start(true, true));
        assert!(can_start(true, false));
    }

    #[test]
    fn monitor_selection_waits_for_enabled_and_uncovered_displays() {
        // Disabled displays are removed by sync_screens before this eligibility check.
        let monitors = [(false, false, false), (true, true, false), (true, false, true), (true, false, false)];
        let choices: Vec<_> = monitors
            .iter()
            .filter(|(enabled, _, _)| *enabled)
            .map(|(_, covered, full_screen)| monitor_eligible(true, *covered, *full_screen))
            .collect();
        assert_eq!(crate::traffic::choose_monitor(&choices, 0), Some(2));
        assert_eq!(crate::traffic::choose_monitor(&choices[..2], 1), None);
        assert!(!monitor_eligible(false, false, false));
    }

    #[test]
    fn flight_does_not_wait_for_ground_traffic() {
        use crate::traffic::{Traffic, Vehicle};
        let mut ground = Traffic::new(0, 0, 0, 1);
        ground.select(Vehicle::Plow, 1, 0);
        ground.select(Vehicle::Truck, 1, 0);
        assert_eq!(ground.poll(0, 1, true, false), Some(Vehicle::Plow));
        assert!(can_start(true, false));
        assert_eq!(ground.poll(1, 1, true, true), None);
        assert_eq!(ground.poll(2, 1, true, false), None);
        assert_eq!(ground.poll(91, 1, true, false), None);
        assert_eq!(ground.poll(92, 1, true, false), Some(Vehicle::Truck));
    }

    #[test]
    fn flight_starts_and_finishes_offscreen_and_curves_smoothly_both_ways() {
        for random in [0x123456789abcu64, 0xabcdef123457] {
            let mut s = Sleigh::new(800.0, 600.0, 1920, 1080, random);
            let margin = s.sprite_width * 0.53;
            assert!(s.x + margin <= 0.0 || s.x - margin >= 800.0);
            let initial_y = s.y;
            let mut exited = false;
            for _ in 0..1000 {
                let (x, y) = (s.x, s.y);
                exited = s.advance(0.05);
                assert!((s.x - x) * s.direction > 0.0);
                assert!((s.y - y).abs() < 1.0);
                if exited {
                    break;
                }
            }
            assert!(exited && (s.y - initial_y).abs() > 1.0);
            assert!(s.x + margin <= 0.0 || s.x - margin >= 800.0);
        }
    }
}
