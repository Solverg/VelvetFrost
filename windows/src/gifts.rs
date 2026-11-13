//! Gift behavior, persistence and interaction.
pub const LANTERN_RADIUS: f32 = 250.0;
pub const LANTERN_MELT_AREA_PER_SECOND: f32 = 150.0;
use snowhush_core::geometry::V2;

/// Each kind can occur at most once on a monitor, including while its gift is opening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiftKind {
    Boombox,
    Lantern,
}

/// Wrapping is chosen separately from the reward and stays fixed until opening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GiftColor {
    Red,
    Green,
}

impl GiftColor {
    pub fn from_random(random: u64) -> Self {
        if random & 1 == 0 { Self::Red } else { Self::Green }
    }
}

const GIFT_KINDS: &[GiftKind] = &[GiftKind::Boombox, GiftKind::Lantern];

/// Select among kinds not already present on this monitor.
pub fn available_kind(gifts: &[Gift], random: u64) -> Option<GiftKind> {
    let available =
        || GIFT_KINDS.iter().copied().filter(|kind| gifts.iter().all(|gift| gift.kind != *kind || gift.broken.is_some() || gift.size <= 0.0));
    let count = available().count();
    if count == 0 { None } else { available().nth((random % count as u64) as usize) }
}

/// A gift becomes a separate object as soon as it appears under a tree.
#[derive(Clone)]
pub struct Gift {
    pub kind: GiftKind,
    pub color: GiftColor,
    pub enabled: bool,
    pub stored: bool,
    pub x: f32,
    pub bottom: f32,
    pub size: f32,
    pub support: isize,
    pub offset: f32,
    pub tilt: f32,
    pub age: f32,
    pub opening: Option<f32>,
    pub revealed: bool,
    /// Tree identity and stable slot id, if this is hanging from an adult tree.
    pub attachment: Option<(u64, u8)>,
    pub pending_window: Option<(String, f32)>,
    pub broken: Option<f32>,
    velocity: f32,
    terrain_initialized: bool,
}

impl Gift {
    pub fn is_lit_lantern(&self) -> bool {
        self.kind == GiftKind::Lantern && self.revealed && self.enabled && !self.stored && self.size > 0.0 && self.broken.is_none()
    }
    #[cfg(test)]
    pub fn new(x: f32, bottom: f32, size: f32, support: isize, offset: f32) -> Self {
        Self::with_kind(GiftKind::Boombox, GiftColor::Red, x, bottom, size, support, offset)
    }

    pub fn with_kind(kind: GiftKind, color: GiftColor, x: f32, bottom: f32, size: f32, support: isize, offset: f32) -> Self {
        Self {
            kind,
            color,
            enabled: true,
            stored: false,
            x,
            bottom,
            size,
            support,
            offset,
            tilt: 0.0,
            age: 0.0,
            opening: None,
            revealed: false,
            attachment: None,
            pending_window: None,
            broken: None,
            velocity: 0.0,
            terrain_initialized: false,
        }
    }

    pub fn advance(&mut self, dt: f32) -> bool {
        if !dt.is_finite() || dt <= 0.0 {
            return false;
        }
        let dt = dt.min(0.05);
        let mut changed = self.age < 0.75;
        self.age = (self.age + dt).min(0.75);
        if let Some(elapsed) = &mut self.opening {
            *elapsed += dt;
            changed = true;
            if *elapsed >= 1.0 {
                self.opening = None;
                self.revealed = true;
            }
        }
        changed
    }

    pub fn open(&mut self) {
        if self.opening.is_none() && !self.revealed {
            self.opening = Some(0.0);
        }
    }

    pub fn begin_fall(&mut self) {
        self.attachment = None;
        self.pending_window = None;
        self.support = -1;
        self.velocity = 0.0;
        self.terrain_initialized = true;
    }

    /// Gravity and the object break threshold match the sculpture physics.
    /// Returns (landed, broken). Caller selects the first surface below the object.
    pub fn fall(&mut self, dt: f32, surface: f32) -> (bool, bool) {
        if !dt.is_finite() || dt <= 0.0 || !surface.is_finite() {
            return (false, false);
        }
        let dt = dt.min(0.05);
        self.velocity += 900.0 * dt;
        self.bottom -= self.velocity * dt;
        if self.bottom <= surface {
            self.bottom = surface;
            let broken = self.revealed && self.velocity > snowhush_core::snowmen::OBJECT_BREAK_SPEED;
            self.velocity = 0.0;
            (true, broken)
        } else {
            (false, false)
        }
    }

    pub fn hit(&self, at: V2) -> bool {
        if self.broken.is_some() {
            return false;
        }
        let (sin, cos) = self.tilt.sin_cos();
        let centre = V2::new(self.x - sin * self.size * 0.5, self.bottom + cos * self.size * 0.5);
        let p = at - centre;
        let local = V2::new(p.x * cos + p.y * sin, -p.x * sin + p.y * cos);
        let width = if self.revealed && self.kind == GiftKind::Lantern {
            0.56
        } else if self.revealed {
            0.80
        } else {
            0.62
        };
        local.x.abs() <= self.size * width && local.y.abs() <= self.size * 0.62
    }

    /// Sample the snow beneath both ends, as with the truck; fall when it drops away.
    pub fn follow_terrain(&mut self, dt: f32, left: f32, right: f32) -> bool {
        if !dt.is_finite() || dt <= 0.0 || !left.is_finite() || !right.is_finite() {
            return false;
        }
        let before = (self.bottom, self.tilt);
        let dt = dt.min(0.05);
        let target = (left + right) * 0.5;
        let slope = ((right - left) / (self.size * 0.75)).atan().clamp(-0.35, 0.35);
        let blend = if self.terrain_initialized { 1.0 - (-7.0 * dt).exp() } else { 1.0 };
        self.tilt += (slope - self.tilt) * blend;
        if !self.terrain_initialized || target >= self.bottom {
            self.bottom = target;
            self.velocity = 0.0;
        } else {
            self.velocity += 900.0 * dt;
            self.bottom = (self.bottom - self.velocity * dt).max(target);
            if self.bottom <= target {
                self.velocity = 0.0;
            }
        }
        self.terrain_initialized = true;
        (self.bottom - before.0).abs() > 0.001 || (self.tilt - before.1).abs() > 0.0001
    }

    /// Scale, vertical bounce, and opacity for a short entrance, anchored at the bottom.
    pub fn entrance(&self) -> (f32, f32, f32) {
        let t = (self.age / 0.75).clamp(0.0, 1.0);
        let scale = if t >= 1.0 { 1.0 } else { 1.0 - (-7.0 * t).exp() * (10.0 * t).cos() };
        let bounce = (std::f32::consts::PI * t).sin().max(0.0) * (1.0 - t) * 0.12;
        (scale.max(0.01), bounce, (t * 6.0).min(1.0))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum SavedSupport {
    Ground,
    Falling,
    Stored,
    Tree(u64, u8),
    Window(String, f32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedGift {
    pub kind: GiftKind,
    pub enabled: bool,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub support: SavedSupport,
}

pub fn encode_saved(items: &[SavedGift]) -> String {
    use std::fmt::Write;
    let mut value =
        String::from(if items.iter().any(|item| item.support == SavedSupport::Stored) { "snowhush-gifts-3\n" } else { "snowhush-gifts-2\n" });
    for item in items {
        let kind = match item.kind {
            GiftKind::Boombox => "b",
            GiftKind::Lantern => "l",
        };
        let _ = write!(value, "{} {} {:.6} {:.6} {:.3} ", kind, u8::from(item.enabled), item.x, item.y, item.size);
        match &item.support {
            SavedSupport::Ground => value.push_str("g\n"),
            SavedSupport::Falling => value.push_str("f\n"),
            SavedSupport::Stored => value.push_str("s\n"),
            SavedSupport::Tree(tree, slot) => {
                let _ = writeln!(value, "t {tree} {slot}");
            }
            SavedSupport::Window(key, fraction) => {
                let _ = writeln!(value, "w {:.6} {}", fraction, crate::sculpt::hex(key.as_bytes()));
            }
        }
    }
    value
}

pub fn decode_saved(value: &str) -> Option<Vec<SavedGift>> {
    let mut lines = value.lines();
    let version = match lines.next()? {
        "snowhush-boomboxes-1" => 1,
        "snowhush-gifts-2" => 2,
        "snowhush-gifts-3" => 3,
        _ => return None,
    };
    let mut result = Vec::new();
    for line in lines {
        if result.len() >= 64 {
            return None;
        }
        let mut parts = line.split_whitespace();
        let (kind, enabled) = if version == 1 {
            (GiftKind::Boombox, true)
        } else {
            let kind = match parts.next()? {
                "b" => GiftKind::Boombox,
                "l" => GiftKind::Lantern,
                _ => return None,
            };
            let enabled = match parts.next()? {
                "0" => false,
                "1" => true,
                _ => return None,
            };
            (kind, enabled)
        };
        let x: f32 = parts.next()?.parse().ok()?;
        let y: f32 = parts.next()?.parse().ok()?;
        let size: f32 = parts.next()?.parse().ok()?;
        if !x.is_finite()
            || !y.is_finite()
            || !size.is_finite()
            || !(-1.0..=2.0).contains(&x)
            || !(-1.0..=2.0).contains(&y)
            || !(10.0..=100.0).contains(&size)
        {
            return None;
        }
        let support = match parts.next()? {
            "g" => SavedSupport::Ground,
            "f" => SavedSupport::Falling,
            "s" if version >= 3 => SavedSupport::Stored,
            "t" => {
                let id = parts.next()?.parse().ok()?;
                let slot: u8 = parts.next()?.parse().ok()?;
                if slot > 1 {
                    return None;
                }
                SavedSupport::Tree(id, slot)
            }
            "w" => {
                let fraction: f32 = parts.next()?.parse().ok()?;
                if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                    return None;
                }
                let key = String::from_utf8(crate::sculpt::unhex(parts.next()?)?).ok()?;
                if key.is_empty() {
                    return None;
                }
                SavedSupport::Window(key, fraction)
            }
            _ => return None,
        };
        if parts.next().is_some() {
            return None;
        }
        result.push(SavedGift { kind, enabled, x, y, size, support });
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_reward_round_trips_without_losing_lantern_state() {
        let saved = SavedGift { kind: GiftKind::Lantern, enabled: false, x: 0.4, y: 0.2, size: 30.0, support: SavedSupport::Stored };
        let encoded = encode_saved(&[saved.clone()]);
        assert!(encoded.starts_with("snowhush-gifts-3\n"));
        assert_eq!(decode_saved(&encoded), Some(vec![saved]));
        assert!(decode_saved(&encoded.replacen("snowhush-gifts-3", "snowhush-gifts-2", 1)).is_none());
    }

    #[test]
    fn wrapping_color_is_independent_of_the_reward_and_stable_while_opening() {
        for (random, color) in [(0, GiftColor::Red), (1, GiftColor::Green)] {
            let mut gift = Gift::with_kind(GiftKind::Boombox, GiftColor::from_random(random), 10.0, 0.0, 30.0, 0, 0.0);
            assert_eq!(gift.color, color);
            gift.open();
            for _ in 0..20 {
                gift.advance(0.05);
            }
            assert!(gift.revealed);
            assert_eq!(gift.color, color);
            assert_eq!(gift.kind, GiftKind::Boombox);
        }
    }

    #[test]
    fn only_an_open_enabled_unbroken_lantern_gives_heat() {
        let mut gift = Gift::with_kind(GiftKind::Lantern, GiftColor::Red, 10.0, 0.0, 30.0, 0, 0.0);
        assert!(!gift.is_lit_lantern());
        gift.revealed = true;
        assert!(gift.is_lit_lantern());
        gift.enabled = false;
        assert!(!gift.is_lit_lantern());
        gift.enabled = true;
        gift.stored = true;
        assert!(!gift.is_lit_lantern());
        gift.stored = false;
        gift.broken = Some(0.0);
        assert!(!gift.is_lit_lantern());
    }

    #[test]
    fn revealed_lantern_has_a_narrower_hit_area() {
        let mut gift = Gift::with_kind(GiftKind::Lantern, GiftColor::Red, 100.0, 0.0, 30.0, 0, 0.0);
        gift.revealed = true;
        assert!(gift.hit(V2::new(115.0, 15.0)));
        assert!(!gift.hit(V2::new(120.0, 15.0)));
    }

    #[test]
    fn available_kinds_are_scoped_to_each_monitor_and_released_after_breakage() {
        let empty_monitor = Vec::new();
        let mut occupied_monitor = vec![Gift::new(10.0, 0.0, 30.0, 0, 0.0)];
        assert_eq!(available_kind(&empty_monitor, 0), Some(GiftKind::Boombox));
        // Reserve the kind as soon as the gift appears, including while it opens.
        assert_eq!(available_kind(&occupied_monitor, 0), Some(GiftKind::Lantern));
        occupied_monitor[0].open();
        assert_eq!(available_kind(&occupied_monitor, 0), Some(GiftKind::Lantern));
        occupied_monitor[0].revealed = true;
        occupied_monitor[0].opening = None;
        occupied_monitor[0].begin_fall();
        assert_eq!(available_kind(&occupied_monitor, 0), Some(GiftKind::Lantern));
        assert_eq!(available_kind(&empty_monitor, 0), Some(GiftKind::Boombox));
        occupied_monitor.push(Gift::with_kind(GiftKind::Lantern, GiftColor::Green, 30.0, 0.0, 30.0, 0, 0.0));
        assert_eq!(available_kind(&occupied_monitor, 0), None);
        occupied_monitor[0].broken = Some(0.0);
        assert_eq!(available_kind(&occupied_monitor, 0), Some(GiftKind::Boombox));
    }

    #[test]
    fn entrance_settles_once_without_continuous_redraw() {
        let mut gift = Gift::new(100.0, 0.0, 40.0, 0, 0.0);
        assert_eq!(gift.entrance().2, 0.0);
        for _ in 0..15 {
            assert!(gift.advance(0.05));
        }
        let (scale, bounce, alpha) = gift.entrance();
        assert_eq!((scale, bounce, alpha), (1.0, 0.0, 1.0));
        assert!(!gift.advance(0.05));
    }

    #[test]
    fn gift_follows_slopes_falls_and_opens_only_once() {
        let mut gift = Gift::new(100.0, 0.0, 40.0, 0, 0.0);
        assert!(gift.follow_terrain(0.05, 8.0, 12.0));
        assert_eq!(gift.bottom, 10.0);
        assert!(gift.tilt > 0.0);
        assert!(gift.hit(V2::new(100.0, 30.0)));
        gift.follow_terrain(0.05, 0.0, 0.0);
        assert!(gift.bottom > 0.0 && gift.bottom < 10.0);
        for _ in 0..40 {
            gift.follow_terrain(0.05, 0.0, 0.0);
        }
        assert_eq!(gift.bottom, 0.0);
        gift.open();
        for _ in 0..20 {
            gift.advance(0.05);
        }
        assert!(gift.revealed);
        gift.open();
        assert!(gift.opening.is_none());
    }

    #[test]
    fn low_fall_lands_and_high_fall_breaks_only_a_revealed_boombox() {
        let mut low = Gift::new(30.0, 20.0, 30.0, 0, 0.0);
        low.revealed = true;
        low.begin_fall();
        let mut result = (false, false);
        for _ in 0..100 {
            result = low.fall(0.02, 0.0);
            if result.0 {
                break;
            }
        }
        assert_eq!(result, (true, false));
        assert_eq!(low.bottom, 0.0);
        let mut high = Gift::new(30.0, 300.0, 30.0, 0, 0.0);
        high.revealed = true;
        high.begin_fall();
        for _ in 0..100 {
            result = high.fall(0.02, 0.0);
            if result.0 {
                break;
            }
        }
        assert_eq!(result, (true, true));
    }

    #[test]
    fn revealed_items_break_at_three_times_the_previous_fall_height() {
        for kind in [GiftKind::Boombox, GiftKind::Lantern] {
            for (height, revealed, should_break) in [(212.0, true, false), (220.0, true, true), (220.0, false, false)] {
                let mut gift = Gift::with_kind(kind, GiftColor::Red, 30.0, height, 30.0, 0, 0.0);
                gift.revealed = revealed;
                gift.begin_fall();
                let mut result = (false, false);
                for _ in 0..1000 {
                    result = gift.fall(0.001, 0.0);
                    if result.0 {
                        break;
                    }
                }
                assert_eq!(result, (true, should_break), "{kind:?} from {height}, revealed {revealed}");
            }
        }
    }

    #[test]
    fn saved_boomboxes_round_trip_and_reject_invalid_data() {
        let items = vec![
            SavedGift { kind: GiftKind::Boombox, enabled: true, x: 0.25, y: 0.1, size: 32.0, support: SavedSupport::Ground },
            SavedGift { kind: GiftKind::Lantern, enabled: false, x: 0.5, y: 0.4, size: 40.0, support: SavedSupport::Tree(1234567, 1) },
            SavedGift {
                kind: GiftKind::Boombox,
                enabled: true,
                x: 0.7,
                y: 0.6,
                size: 35.0,
                support: SavedSupport::Window("app\nclass\ntitle".into(), 0.5),
            },
            SavedGift { kind: GiftKind::Boombox, enabled: true, x: 0.6, y: 0.7, size: 30.0, support: SavedSupport::Falling },
        ];
        assert_eq!(decode_saved(&encode_saved(&items)), Some(items.clone()));
        assert_eq!(decode_saved("snowhush-boomboxes-1\n0.25 0.1 32 g\n"), Some(vec![items[0].clone()]));
        assert!(decode_saved("snowhush-gifts-2\nl 2 0.2 0.2 30 g\n").is_none());
        assert!(decode_saved("future-format\n").is_none());
        assert!(decode_saved("snowhush-boomboxes-1\nNaN 0.2 30 g\n").is_none());
        assert!(decode_saved("snowhush-boomboxes-1\n0.2 0.2 30 t 123 7\n").is_none());
        assert!(decode_saved("snowhush-boomboxes-1\n0.2 0.2 30 w 0.5 invalid\n").is_none());
    }
}
