//! One pass along the bottom of a display. Coordinates are in the snow simulation's points.

const LARGE_WIDTH: f32 = 120.0;
const SMALL_WIDTH: f32 = 80.0;
pub const SPEED: f32 = 42.0;
// The new 1774×887 artwork has a longer silhouette. Accounting for transparent
// margins, this makes its visible height 15% greater than the old artwork.
const PLOW_WIDTH_SCALE: f32 = 1.603;
pub const BLADE_OFFSET: f32 = 0.48;

/// Choose by the monitor's physical resolution, regardless of its orientation or taskbar size.
pub fn sprite_width(physical_width: i32, physical_height: i32) -> f32 {
    if physical_width.max(physical_height) <= 1920 && physical_width.min(physical_height) <= 1200 { SMALL_WIDTH } else { LARGE_WIDTH }
}

#[derive(Clone, Copy, Debug)]
pub struct Plow {
    pub x: f32,
    pub direction: f32,
    pub age: f32,
    pub sprite_width: f32,
    width: f32,
}

impl Plow {
    pub fn new(width: f32, physical_width: i32, physical_height: i32, right: bool) -> Self {
        let direction = if right { 1.0 } else { -1.0 };
        let sprite_width = sprite_width(physical_width, physical_height) * PLOW_WIDTH_SCALE;
        let x = if right { -sprite_width / 2.0 } else { width + sprite_width / 2.0 };
        Self { x, direction, age: 0.0, sprite_width, width }
    }

    /// Advance and return the blade's previous and current position and whether the vehicle has exited.
    pub fn advance(&mut self, dt: f32) -> (f32, f32, bool) {
        // The blade reaches nearly to the end of the artwork, with a transparent margin.
        let from = self.x + self.direction * self.sprite_width * BLADE_OFFSET;
        self.x += self.direction * SPEED * dt;
        self.age += dt;
        let to = self.x + self.direction * self.sprite_width * BLADE_OFFSET;
        let exited = if self.direction > 0.0 { self.x - self.sprite_width / 2.0 >= self.width } else { self.x + self.sprite_width / 2.0 <= 0.0 };
        (from, to, exited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_across_and_exits_in_both_directions() {
        for right in [true, false] {
            let mut plow = Plow::new(800.0, 1920, 1200, right);
            let first = plow.x + plow.direction * plow.sprite_width * BLADE_OFFSET;
            let mut last = first;
            let mut finished = false;
            for _ in 0..1000 {
                let (from, to, exited) = plow.advance(0.05);
                assert!((from - last).abs() < 0.001);
                assert_eq!((to - from).signum(), plow.direction);
                last = to;
                if exited {
                    finished = true;
                    break;
                }
            }
            assert!(finished);
            assert!(if right { first < 0.0 && last > 800.0 } else { first > 800.0 && last < 0.0 });
        }
    }

    #[test]
    fn smaller_variant_is_selected_per_monitor_resolution() {
        assert_eq!(sprite_width(1920, 1200), SMALL_WIDTH);
        assert_eq!(sprite_width(1920, 1080), SMALL_WIDTH);
        assert_eq!(sprite_width(1200, 1920), SMALL_WIDTH);
        assert_eq!(sprite_width(1280, 720), SMALL_WIDTH);
        assert_eq!(sprite_width(2560, 1440), LARGE_WIDTH);
        assert_eq!(sprite_width(2560, 1080), LARGE_WIDTH);
    }
}
