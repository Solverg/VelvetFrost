//! A small winter visitor: flies in, hops on the snow, then flies away.
use snowhush_core::geometry::V2;

// The visible bird occupies roughly two thirds of this quad and is much smaller than the fox.
pub const SIZE: f32 = 42.0;
const FOOT_OFFSET: f32 = SIZE * 0.25;
const VISIT: f32 = 18.0;
const NOTICE: f32 = 115.0;
const STARTLE: f32 = 55.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Arriving { elapsed: f32, start: V2 },
    Perched,
    Departing,
}

pub struct Bullfinch {
    pub position: V2,
    target_x: f32,
    direction: f32,
    age: f32,
    flight_age: f32,
    hop: Option<(f32, f32, f32)>,
    phase: Phase,
}

impl Bullfinch {
    pub fn new(x: f32, ground: f32, width: f32, height: f32, from_left: bool) -> Self {
        let start = V2::new(if from_left { -SIZE } else { width + SIZE }, (ground + 160.0).min(height + SIZE));
        Self {
            position: start,
            target_x: x,
            direction: if from_left { 1.0 } else { -1.0 },
            age: 0.0,
            flight_age: 0.0,
            hop: None,
            phase: Phase::Arriving { elapsed: 0.0, start },
        }
    }

    fn depart(&mut self, away_from: f32, width: f32) {
        self.direction = if (away_from - self.position.x).abs() < 1.0 {
            if self.position.x < width * 0.5 { -1.0 } else { 1.0 }
        } else if away_from < self.position.x {
            1.0
        } else {
            -1.0
        };
        self.hop = None;
        self.flight_age = 0.0;
        self.phase = Phase::Departing;
    }

    /// `supported` also covers mode changes and newly obstructed snow.
    pub fn advance(
        &mut self,
        dt: f32,
        cursor: V2,
        width: f32,
        height: f32,
        supported: bool,
        danger: bool,
        ground: impl Fn(f32) -> f32,
        can_step: impl Fn(f32) -> bool,
    ) -> bool {
        if !dt.is_finite() || dt <= 0.0 {
            return false;
        }
        let dt = dt.min(0.05);
        let distance = (cursor - self.position).length();
        if !matches!(self.phase, Phase::Departing) && (!supported || danger || distance < STARTLE) {
            self.depart(if distance < STARTLE { cursor.x } else { self.position.x }, width);
        }
        match self.phase {
            Phase::Arriving { elapsed, start } => {
                self.flight_age += dt;
                let elapsed = (elapsed + dt).min(2.5);
                let t = elapsed / 2.5;
                let smooth = t * t * (3.0 - 2.0 * t);
                self.position =
                    V2::new(start.x + (self.target_x - start.x) * smooth, start.y + (ground(self.target_x) + FOOT_OFFSET - start.y) * smooth);
                if elapsed >= 2.5 {
                    self.phase = Phase::Perched;
                } else {
                    self.phase = Phase::Arriving { elapsed, start };
                }
            }
            Phase::Perched => {
                let previous = self.age;
                self.age += dt;
                if self.age >= VISIT {
                    self.depart(self.position.x, width);
                } else if let Some((elapsed, from, to)) = self.hop {
                    let elapsed = (elapsed + dt).min(0.55);
                    let t = elapsed / 0.55;
                    self.position.x = from + (to - from) * t;
                    self.position.y = ground(self.position.x) + FOOT_OFFSET + (std::f32::consts::PI * t).sin() * 8.0;
                    self.hop = (elapsed < 0.55).then_some((elapsed, from, to));
                } else {
                    self.position.y = ground(self.position.x) + FOOT_OFFSET;
                    if distance < NOTICE {
                        self.direction = if cursor.x < self.position.x { -1.0 } else { 1.0 };
                    } else if (previous / 4.0).floor() < (self.age / 4.0).floor() {
                        let to = self.position.x + self.direction * 20.0;
                        if can_step(to) {
                            self.hop = Some((0.0, self.position.x, to));
                        } else {
                            self.direction = -self.direction;
                        }
                    }
                }
            }
            Phase::Departing => {
                self.flight_age += dt;
                self.position.x += self.direction * 140.0 * dt;
                self.position.y += 100.0 * dt;
                return self.position.x < -SIZE || self.position.x > width + SIZE || self.position.y > height + SIZE;
            }
        }
        false
    }

    pub fn frame(&self) -> f32 {
        if !matches!(self.phase, Phase::Perched) && (self.flight_age * 7.0) as u32 % 2 == 0 { 1.0 } else { 0.0 }
    }

    pub fn facing_right(&self) -> bool {
        self.direction > 0.0
    }

    pub fn support_x(&self) -> f32 {
        if matches!(self.phase, Phase::Arriving { .. }) { self.target_x } else { self.position.x }
    }

    /// A gentle peck during pauses; in flight the whole bird tilts slightly.
    pub fn tilt(&self) -> f32 {
        if matches!(self.phase, Phase::Perched) && self.hop.is_none() && self.age % 5.0 > 3.8 {
            (self.age * 7.0).sin().max(0.0) * 0.16 * self.direction
        } else if !matches!(self.phase, Phase::Perched) {
            -0.08 * self.direction
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FAR: V2 = V2 { x: -1000.0, y: -1000.0 };
    fn step(bird: &mut Bullfinch, cursor: V2, supported: bool, danger: bool) -> bool {
        bird.advance(0.05, cursor, 800.0, 600.0, supported, danger, |_| 20.0, |_| true)
    }

    #[test]
    fn arrives_hops_and_leaves_after_the_visit() {
        let mut bird = Bullfinch::new(400.0, 20.0, 800.0, 600.0, true);
        assert!(bird.position.x < 0.0);
        let mut seen = [false; 2];
        while !matches!(bird.phase, Phase::Perched) {
            seen[bird.frame() as usize] = true;
            assert!(!step(&mut bird, FAR, true, false));
        }
        assert!(seen.into_iter().all(|v| v));
        let x = bird.position.x;
        for _ in 0..100 {
            assert!(!step(&mut bird, FAR, true, false));
        }
        assert!(bird.position.x > x);
        for _ in 0..600 {
            if step(&mut bird, FAR, true, false) {
                return;
            }
        }
        panic!("bird did not leave");
    }

    #[test]
    fn watches_cursor_and_flies_away_from_it() {
        let mut bird = Bullfinch::new(400.0, 20.0, 800.0, 600.0, true);
        while !matches!(bird.phase, Phase::Perched) {
            step(&mut bird, FAR, true, false);
        }
        let p = bird.position;
        step(&mut bird, V2::new(p.x - 80.0, p.y), true, false);
        assert!(!bird.facing_right());
        step(&mut bird, V2::new(p.x - 10.0, p.y), true, false);
        assert!(matches!(bird.phase, Phase::Departing));
        assert!(bird.facing_right());
    }

    #[test]
    fn lost_snow_or_vehicle_causes_departure_even_during_arrival() {
        for (support, danger) in [(false, false), (true, true)] {
            let mut bird = Bullfinch::new(400.0, 20.0, 800.0, 600.0, false);
            step(&mut bird, FAR, support, danger);
            assert!(matches!(bird.phase, Phase::Departing));
        }
    }
}
