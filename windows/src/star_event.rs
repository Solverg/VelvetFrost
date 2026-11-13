//! The golden snowflake event.
use snowhush_core::geometry::V2;

pub const BASE_COUNT: u8 = 10;
pub const HIT_RADIUS: f32 = 30.0;
const FALL_SPEED: f32 = 38.0;
pub const LEGENDARY_VARIANTS: [u8; 3] = [crate::sculpt::CELESTIAL_VARIANT, crate::sculpt::RUBY_VARIANT, crate::sculpt::ROSE_VARIANT];

pub fn owner_key(variant: u8) -> &'static str {
    match variant {
        crate::sculpt::CELESTIAL_VARIANT => "celestial-owner",
        crate::sculpt::RUBY_VARIANT => "ruby-owner",
        crate::sculpt::ROSE_VARIANT => "rose-owner",
        _ => unreachable!("unknown legendary tree"),
    }
}

pub fn choose_reward(available: &[u8], random: u64) -> Option<u8> {
    let weight = |variant| match variant {
        crate::sculpt::CELESTIAL_VARIANT => 80,
        crate::sculpt::RUBY_VARIANT | crate::sculpt::ROSE_VARIANT => 10,
        _ => 0,
    };
    let total: u64 = available.iter().map(|&variant| weight(variant)).sum();
    if total == 0 {
        return None;
    }
    let mut draw = random % total;
    for &variant in available {
        let weight = weight(variant);
        if draw < weight {
            return Some(variant);
        }
        draw -= weight;
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
pub enum OwnerState {
    Available,
    Occupied,
    Stale,
}

/// An absent owner display may be disconnected, so never assume its tree was lost.
pub fn owner_state(owner: Option<&str>, displays: &[(&str, bool)]) -> OwnerState {
    match owner.filter(|owner| !owner.is_empty()) {
        None => OwnerState::Available,
        Some(owner) => match displays.iter().find(|(name, _)| *name == owner) {
            Some((_, false)) => OwnerState::Stale,
            _ => OwnerState::Occupied,
        },
    }
}

pub struct StarChain {
    pub position: V2,
    pub caught: u8,
    pub target: u8,
    pub reward: u8,
    speed: f32,
    seed: u64,
}

impl StarChain {
    pub fn new(width: f32, height: f32, seed: u64, owned: usize, reward: u8) -> Self {
        let mut chain = Self {
            position: V2::ZERO,
            caught: 0,
            target: BASE_COUNT.saturating_add(owned.min(u8::MAX as usize) as u8),
            reward,
            speed: FALL_SPEED * (1.0 + owned as f32 * 0.1),
            seed,
        };
        chain.next_position(width, height);
        chain
    }

    fn next_position(&mut self, width: f32, height: f32) {
        self.seed = self.seed.wrapping_add(0x9e3779b97f4a7c15);
        let mut random = self.seed;
        random = (random ^ (random >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        random = (random ^ (random >> 27)).wrapping_mul(0x94d049bb133111eb);
        random ^= random >> 31;
        let fraction = (random as u32 as f64 / u32::MAX as f64) as f32;
        let margin = HIT_RADIUS.min(width * 0.5);
        self.position = V2::new(margin + fraction * (width - 2.0 * margin).max(0.0), (height - HIT_RADIUS).max(0.0));
    }

    pub fn advance(&mut self, dt: f32) -> bool {
        if dt.is_finite() && dt > 0.0 {
            self.position.y -= self.speed * dt.min(0.05);
        }
        self.position.y > 0.0
    }

    pub fn hit(&self, point: V2) -> bool {
        (self.position - point).length_squared() <= HIT_RADIUS * HIT_RADIUS
    }

    /// Returns true after the tenth catch; otherwise immediately releases the next flake.
    pub fn catch(&mut self, width: f32, height: f32) -> bool {
        self.caught += 1;
        if self.caught >= self.target {
            true
        } else {
            self.next_position(width, height);
            false
        }
    }

    pub fn reward_seed(&self) -> u64 {
        self.seed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_catches_complete_and_missing_a_flake_ends_the_chain() {
        let mut chain = StarChain::new(800.0, 600.0, 1, 0, 4);
        assert!(chain.hit(chain.position));
        assert!(!chain.hit(chain.position + V2::new(HIT_RADIUS + 1.0, 0.0)));
        for caught in 1..=BASE_COUNT {
            assert_eq!(chain.catch(800.0, 600.0), caught == BASE_COUNT);
            assert_eq!(chain.caught, caught);
        }
        let mut missed = StarChain::new(800.0, 600.0, 2, 0, 4);
        missed.position.y = 1.0;
        assert!(!missed.advance(0.05));
    }

    #[test]
    fn difficulty_and_reward_selection_follow_owned_trees() {
        for owned in 0..=3 {
            let mut chain = StarChain::new(800.0, 600.0, 1, owned, 5);
            assert_eq!(chain.target, 10 + owned as u8);
            let before = chain.position.y;
            chain.advance(0.05);
            assert!((before - chain.position.y - 38.0 * (1.0 + owned as f32 * 0.1) * 0.05).abs() < 0.0001);
            for n in 1..=chain.target {
                assert_eq!(chain.catch(800.0, 600.0), n == chain.target);
            }
        }
    }

    #[test]
    fn rewards_use_weights_of_available_designs() {
        let cases: &[(&[u8], &[(u8, usize)])] = &[
            (&[4, 5, 6], &[(4, 80), (5, 10), (6, 10)]),
            (&[4, 5], &[(4, 80), (5, 10)]),
            (&[4, 6], &[(4, 80), (6, 10)]),
            (&[5, 6], &[(5, 10), (6, 10)]),
            (&[4], &[(4, 80)]),
            (&[5], &[(5, 10)]),
            (&[6], &[(6, 10)]),
        ];
        for &(available, expected) in cases {
            let total: usize = expected.iter().map(|(_, count)| count).sum();
            for &(variant, count) in expected {
                assert_eq!((0..total as u64).filter(|&draw| choose_reward(available, draw) == Some(variant)).count(), count);
            }
        }
        assert_eq!(choose_reward(&[], 0), None);
    }

    #[test]
    fn saved_owner_blocks_duplicate_when_monitor_is_disconnected() {
        let displays = [("left", true), ("right", false)];
        assert_eq!(owner_state(None, &displays), OwnerState::Available);
        assert_eq!(owner_state(Some("left"), &displays), OwnerState::Occupied);
        assert_eq!(owner_state(Some("unplugged"), &displays), OwnerState::Occupied);
        assert_eq!(owner_state(Some("right"), &displays), OwnerState::Stale);
    }
}
