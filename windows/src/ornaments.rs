//! Shared geometry for future decorations attached to adult trees.
use crate::cone::Species;
use snowhush_core::geometry::V2;

/// Attachment centres were chosen on branches below the crown decorations. IDs stay
/// stable across releases because saved decorations refer to them.
pub fn tree_slots(species: Species, variant: u8) -> [Slot; 2] {
    // New colour variants 2 and 3 follow the branch geometry of variants 0 and 1.
    let positions = match (species, variant) {
        (Species::Spruce, crate::sculpt::CELESTIAL_VARIANT) => [(-0.11, 0.36), (0.11, 0.36)],
        (Species::Spruce, crate::sculpt::RUBY_VARIANT) => [(-0.11, 0.36), (0.11, 0.36)],
        (species, variant) => match (species, variant.min(3) % 2) {
            (Species::Spruce, 0) => [(-0.11, 0.39), (0.11, 0.39)],
            (Species::Spruce, _) => [(-0.10, 0.40), (0.10, 0.40)],
            (Species::Pine, 0) => [(-0.11, 0.43), (0.11, 0.43)],
            (Species::Pine, _) => [(-0.10, 0.42), (0.10, 0.42)],
            (Species::Fir, 0) => [(-0.11, 0.40), (0.11, 0.40)],
            (Species::Fir, _) => [(-0.10, 0.38), (0.10, 0.38)],
        },
    };
    positions.map(|(x, y)| Slot { id: if x < 0.0 { 0 } else { 1 }, position: V2::new(x, y), max_size: V2::new(0.52, 0.35) })
}

/// Stable within a tree design. Position and available size are relative to tree height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub id: u8,
    pub position: V2,
    pub max_size: V2,
}

impl Slot {
    pub fn world_position(self, tree_base: V2, tree_height: f32) -> V2 {
        tree_base + self.position * tree_height
    }
}

/// Find a free slot large enough for an item, within the specified snapping distance.
/// `size` and `snap_radius` are in the same screen coordinates as `pointer`.
pub fn nearest_slot(slots: &[Slot], occupied: &[u8], tree_base: V2, tree_height: f32, pointer: V2, size: V2, snap_radius: f32) -> Option<u8> {
    if !tree_height.is_finite()
        || tree_height <= 0.0
        || !snap_radius.is_finite()
        || snap_radius < 0.0
        || !size.x.is_finite()
        || !size.y.is_finite()
        || size.x <= 0.0
        || size.y <= 0.0
        || !pointer.x.is_finite()
        || !pointer.y.is_finite()
    {
        return None;
    }
    slots
        .iter()
        .filter(|slot| !occupied.contains(&slot.id) && size.x <= slot.max_size.x * tree_height && size.y <= slot.max_size.y * tree_height)
        .map(|slot| (slot.id, (slot.world_position(tree_base, tree_height) - pointer).length_squared()))
        .filter(|(_, distance)| *distance <= snap_radius * snap_radius)
        .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_nearest_free_fitting_slot_in_tree_coordinates() {
        let slots = [
            Slot { id: 1, position: V2::new(-0.1, 0.4), max_size: V2::new(0.4, 0.2) },
            Slot { id: 2, position: V2::new(0.1, 0.4), max_size: V2::new(0.2, 0.2) },
        ];
        let base = V2::new(100.0, 20.0);
        let pointer = V2::new(110.0, 60.0);
        assert_eq!(slots[1].world_position(base, 100.0), pointer);
        assert_eq!(nearest_slot(&slots, &[], base, 100.0, pointer, V2::new(15.0, 15.0), 25.0), Some(2));
        assert_eq!(nearest_slot(&slots, &[2], base, 100.0, pointer, V2::new(15.0, 15.0), 25.0), Some(1));
        assert_eq!(nearest_slot(&slots, &[], base, 100.0, pointer, V2::new(30.0, 15.0), 25.0), Some(1));
        assert_eq!(nearest_slot(&slots, &[1], base, 100.0, pointer, V2::new(30.0, 15.0), 25.0), None);
        assert_eq!(nearest_slot(&slots, &[], base, 100.0, pointer, V2::new(15.0, 15.0), 0.0), Some(2));
        assert_eq!(nearest_slot(&slots, &[], base, 0.0, pointer, V2::new(15.0, 15.0), 25.0), None);
    }

    #[test]
    fn all_adult_designs_have_stable_usable_slots() {
        for species in [Species::Spruce, Species::Pine, Species::Fir] {
            for variant in 0..=3 {
                let slots = tree_slots(species, variant);
                assert_eq!([slots[0].id, slots[1].id], [0, 1]);
                for slot in slots {
                    let point = slot.world_position(V2::ZERO, 135.0);
                    assert!(nearest_slot(&slots, &[], V2::ZERO, 135.0, point, V2::new(45.0, 25.0), 1.0).is_some());
                }
            }
        }
        let slots = tree_slots(Species::Spruce, crate::sculpt::CELESTIAL_VARIANT);
        assert_eq!([slots[0].id, slots[1].id], [0, 1]);
        for slot in slots {
            let point = slot.world_position(V2::ZERO, 200.0);
            assert_eq!(nearest_slot(&slots, &[], V2::ZERO, 200.0, point, V2::new(45.0, 25.0), 1.0), Some(slot.id));
        }
    }
}
