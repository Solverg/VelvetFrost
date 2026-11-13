//! A festive truck crossing the same bottom edge as the snowplow.

use crate::plow;

const WHEELS: [f32; 4] = [0.127, 0.204, 0.587, 0.847];
// The road-train texture is split at the coupling, in original image pixels.
pub const TRAIN_SPLIT: f32 = 840.0;
pub const TRAIN_FRONT: f32 = 2172.0 - TRAIN_SPLIT;
const TRAIN_WHEELS: [f32; 4] = [0.45, 0.50, 0.737, 0.905];
const TRAILER_WHEELS: [f32; 2] = [0.071, 0.12];

#[derive(Clone, Copy)]
pub struct Truck {
    pub x: f32,
    pub direction: f32,
    pub age: f32,
    pub sprite_width: f32,
    pub height: f32,
    pub tilt: f32,
    pub rare: bool,
    pub trailer_tilt: f32,
    screen_width: f32,
    terrain_initialized: bool,
}

impl Truck {
    #[cfg(test)]
    pub fn new(width: f32, physical_width: i32, physical_height: i32, right: bool) -> Self {
        Self::new_variant(width, physical_width, physical_height, right, false)
    }

    pub fn new_variant(width: f32, physical_width: i32, physical_height: i32, right: bool, rare: bool) -> Self {
        let direction = if right { 1.0 } else { -1.0 };
        let sprite_width = plow::sprite_width(physical_width, physical_height) * 2.0;
        let x = if right { -sprite_width / 2.0 } else { width + sprite_width / 2.0 };
        Self {
            x,
            direction,
            age: 0.0,
            sprite_width,
            height: 0.0,
            tilt: 0.0,
            rare,
            trailer_tilt: 0.0,
            screen_width: width,
            terrain_initialized: false,
        }
    }

    /// Returns whether to kick up snow this frame and whether the truck has left the display.
    pub fn advance(&mut self, dt: f32) -> (bool, bool) {
        let old_tick = (self.age * 10.0) as u32;
        self.age += dt;
        self.x += self.direction * plow::SPEED * 2.5 * dt;
        let tail = self.sprite_width * (0.5 + if self.rare { TRAIN_SPLIT / TRAIN_FRONT } else { 0.0 });
        let exited = if self.direction > 0.0 { self.x - tail >= self.screen_width } else { self.x + tail <= 0.0 };
        ((self.age * 10.0) as u32 != old_tick, exited)
    }

    pub fn wheel_positions(&self) -> impl Iterator<Item = f32> + '_ {
        let mut wheels = Vec::with_capacity(6);
        if self.rare {
            wheels.extend(TRAIN_WHEELS.map(|x| self.x + ((x * 2172.0 - TRAIN_SPLIT) / TRAIN_FRONT - 0.5) * self.sprite_width * self.direction));
            let (anchor, _) = self.trailer_placement();
            let width = self.sprite_width * TRAIN_SPLIT / TRAIN_FRONT;
            wheels.extend(TRAILER_WHEELS.map(|x| anchor[0] + (x * 2172.0 / TRAIN_SPLIT - 0.5) * width * self.direction * self.trailer_tilt.cos()));
        } else {
            wheels.extend(WHEELS.map(|wheel| self.x + (wheel - 0.5) * self.sprite_width * self.direction));
        }
        wheels.into_iter()
    }

    /// Position the rear segment around the same coupling point as the front segment.
    pub fn trailer_placement(&self) -> ([f32; 2], f32) {
        let rear_width = self.sprite_width * TRAIN_SPLIT / TRAIN_FRONT;
        let height = self.sprite_width * 724.0 / TRAIN_FRONT;
        let joint_y = height * 0.325;
        let front_x = -self.direction * self.sprite_width * 0.5;
        let (s, c) = self.tilt.sin_cos();
        let joint = [self.x + front_x * c - joint_y * s, self.height - height * 0.20 + front_x * s + joint_y * c];
        let rear_x = self.direction * rear_width * 0.5;
        let (s, c) = self.trailer_tilt.sin_cos();
        ([joint[0] - rear_x * c + joint_y * s, joint[1] - rear_x * s - joint_y * c], rear_width)
    }

    /// Let the suspension follow the snow under the rear axle group and the front wheel.
    pub fn follow_terrain(&mut self, dt: f32, surface: impl Fn(f32) -> f32) {
        let wheels: Vec<_> = self.wheel_positions().collect();
        let rear_x = (wheels[0] + wheels[1]) * 0.5;
        let front_x = wheels[3];
        let rear = (surface(wheels[0]) + surface(wheels[1])) * 0.5;
        let front = surface(front_x);
        let tilt = ((front - rear) / (front_x - rear_x)).atan().clamp(-0.16, 0.16);
        let height = (rear + front) * 0.5 + tilt.tan() * (self.x - (rear_x + front_x) * 0.5);
        let blend = if self.terrain_initialized { 1.0 - (-7.0 * dt).exp() } else { 1.0 };
        self.height += (height - self.height) * blend;
        self.tilt += (tilt - self.tilt) * blend;
        if self.rare {
            let (anchor, width) = self.trailer_placement();
            let axle_x = anchor[0] + (0.0955 * 2172.0 / TRAIN_SPLIT - 0.5) * width * self.direction;
            let joint_x = self.x - self.direction * self.sprite_width * 0.5;
            let trailer_tilt = ((surface(joint_x) - surface(axle_x)) / (joint_x - axle_x)).atan().clamp(-0.16, 0.16);
            self.trailer_tilt += (trailer_tilt - self.trailer_tilt) * blend;
        }
        self.terrain_initialized = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travels_two_and_a_half_times_faster_and_exits_both_sides() {
        for right in [true, false] {
            let mut truck = Truck::new(800.0, 1920, 1200, right);
            let start = truck.x;
            truck.advance(0.5);
            assert!(((truck.x - start).abs() - plow::SPEED * 2.5 * 0.5).abs() < 0.001);
            assert_eq!(truck.wheel_positions().count(), 4);
            assert!((0..1000).any(|_| truck.advance(0.05).1));
        }
    }

    #[test]
    fn follows_snow_and_slopes_smoothly_in_either_direction() {
        for right in [true, false] {
            let mut truck = Truck::new(800.0, 1920, 1200, right);
            truck.x = 400.0;
            truck.follow_terrain(0.016, |_| 12.0);
            assert!((truck.height - 12.0).abs() < 0.001);
            assert_eq!(truck.tilt, 0.0);
            truck.follow_terrain(0.016, |x| 12.0 + (x - 400.0) * 0.1);
            assert!(truck.tilt > 0.0 && truck.tilt < 0.1);
            for _ in 0..200 {
                truck.follow_terrain(0.016, |x| 12.0 + (x - 400.0) * 0.1);
            }
            assert!((truck.height - 12.0).abs() < 0.01);
            assert!((truck.tilt - 0.1_f32.atan()).abs() < 0.001);
            truck.follow_terrain(10.0, |x| (x - 400.0) * 10.0);
            assert!(truck.tilt <= 0.16);
        }
    }

    #[test]
    fn road_train_hinges_and_remains_active_until_its_tail_exits() {
        for right in [true, false] {
            let mut truck = Truck::new_variant(800.0, 1920, 1200, right, true);
            truck.x = 400.0;
            truck.follow_terrain(1.0, |_| 0.0);
            assert_eq!(truck.wheel_positions().count(), 6);
            let axle = truck.trailer_placement().0[0];
            truck.follow_terrain(10.0, |x| if (x - axle).abs() < truck_width_for_test() { 20.0 } else { 0.0 });
            assert!((truck.trailer_tilt - truck.tilt).abs() > 0.01);
            let (rear, width) = truck.trailer_placement();
            let h = truck.sprite_width * 724.0 / TRAIN_FRONT;
            let y = h * 0.325;
            let front_joint = [
                truck.x - truck.direction * truck.sprite_width * 0.5 * truck.tilt.cos() - y * truck.tilt.sin(),
                truck.height - h * 0.20 - truck.direction * truck.sprite_width * 0.5 * truck.tilt.sin() + y * truck.tilt.cos(),
            ];
            let rear_joint = [
                rear[0] + truck.direction * width * 0.5 * truck.trailer_tilt.cos() - y * truck.trailer_tilt.sin(),
                rear[1] + truck.direction * width * 0.5 * truck.trailer_tilt.sin() + y * truck.trailer_tilt.cos(),
            ];
            assert!((front_joint[0] - rear_joint[0]).abs() < 0.001);
            assert!((front_joint[1] - rear_joint[1]).abs() < 0.001);
            truck.x = if right { 800.0 + truck.sprite_width * 0.5 } else { -truck.sprite_width * 0.5 };
            assert!(!truck.advance(0.0).1);
            assert!((0..1000).any(|_| truck.advance(0.05).1));
        }
    }

    fn truck_width_for_test() -> f32 {
        50.0
    }
}
