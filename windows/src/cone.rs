//! A single collectible cone.
use snowhush_core::geometry::V2;

const SPEED: f32 = 65.0;
pub const BODY_SIZE: f32 = 38.0;
pub const HIT_RADIUS: f32 = 30.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Species {
    #[default]
    Spruce,
    Pine,
    Fir,
}

impl Species {
    pub fn from_random(random: u64) -> Self {
        match (random >> 32) % 3 {
            0 => Self::Spruce,
            1 => Self::Pine,
            _ => Self::Fir,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Spruce => 0,
            Self::Pine => 1,
            Self::Fir => 2,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Spruce),
            1 => Some(Self::Pine),
            2 => Some(Self::Fir),
            _ => None,
        }
    }
}

/// Use the quotient after choosing the species so all designs are equally likely for each species.
pub fn tree_variant(random: u64) -> u8 {
    (((random >> 32) / 3) % 4) as u8
}

pub struct Cone {
    pub position: V2,
    pub collected: bool,
    pub planted: bool,
    pub collection_time: f32,
    pub random: u64,
    pub species: Species,
    pub legendary: Option<u8>,
}

impl Cone {
    pub fn new(width: f32, height: f32, random: u64) -> Self {
        let fraction = (random as u32 as f64 / u32::MAX as f64) as f32;
        let margin = HIT_RADIUS.min(width * 0.5);
        Self {
            position: V2::new(margin + fraction * (width - 2.0 * margin).max(0.0), height - BODY_SIZE * 0.5),
            collected: false,
            planted: false,
            collection_time: 0.0,
            random,
            species: Species::from_random(random),
            legendary: None,
        }
    }

    pub fn hit(&self, pointer: V2) -> bool {
        !self.collected && (self.position - pointer).length_squared() <= HIT_RADIUS * HIT_RADIUS
    }

    /// Returns the previous and new vertical positions for swept collision detection.
    pub fn advance(&mut self, dt: f32) -> (f32, f32) {
        let previous = self.position.y;
        if dt.is_finite() && dt > 0.0 {
            if self.collected {
                self.collection_time += dt.min(0.05);
            } else {
                self.position.y -= SPEED * dt.min(0.05);
            }
        }
        (previous, self.position.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_position_stays_in_range() {
        let left = Cone::new(1000.0, 700.0, 0);
        let right = Cone::new(1000.0, 700.0, u32::MAX as u64);
        assert_eq!(left.position.y, 700.0 - BODY_SIZE * 0.5);
        assert_eq!(right.position.y, left.position.y);
        assert!(left.position.x < right.position.x);
        assert!(left.hit(left.position));
        assert!(left.hit(left.position + V2::new(HIT_RADIUS - 1.0, 0.0)));
        assert!(!left.hit(left.position + V2::new(HIT_RADIUS + 1.0, 0.0)));
    }

    #[test]
    fn collected_cone_cannot_be_clicked_again() {
        let mut cone = Cone::new(100.0, 100.0, 0);
        cone.collected = true;
        assert!(!cone.hit(cone.position));
        let (before, after) = cone.advance(0.05);
        assert_eq!(after, before);
        assert!(cone.collection_time > 0.0);
    }

    #[test]
    fn species_uses_independent_random_bits_from_horizontal_position() {
        for (i, species) in [Species::Spruce, Species::Pine, Species::Fir].into_iter().enumerate() {
            let cone = Cone::new(1000.0, 700.0, (i as u64) << 32 | 1234);
            assert_eq!(cone.species, species);
            assert_eq!(cone.position.x, Cone::new(1000.0, 700.0, 1234).position.x);
            assert_eq!(Species::from_code(species.code()), Some(species));
        }
    }

    #[test]
    fn all_tree_designs_can_occur_for_each_species() {
        for species in 0..3_u64 {
            for variant in 0..4_u64 {
                let random = (species + 3 * variant) << 32;
                assert_eq!(Species::from_random(random).code(), species as u8);
                assert_eq!(tree_variant(random), variant as u8);
            }
        }
    }
}
