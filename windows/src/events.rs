//! One app-wide lottery for spontaneous events. Weights are chances per three minutes
//! in hundredths of a percent; unused and unavailable outcomes mean no event.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Cone,
    Fox,
    Bullfinch,
    Gift,
    Star,
    Sleigh,
    Ghost,
    Truck,
    RoadTrain,
}

const TOTAL: u64 = 10_000;
// The bullfinch is a common visitor. Slots are unconditional: unavailable
// outcomes are skipped rather than redistributed to the other events.
const WEIGHTS: [(Event, u64); 9] = [
    (Event::Cone, 1043),
    (Event::Fox, 1391),
    (Event::Gift, 348),
    (Event::Star, 1304),
    (Event::Sleigh, 1304),
    (Event::Ghost, 1391),
    (Event::Bullfinch, 1739),
    (Event::Truck, 1080),
    (Event::RoadTrain, 360),
];

pub struct Schedule {
    next: i64,
    interval: i64,
}

impl Schedule {
    pub fn new(now: i64, frequency: i64) -> Self {
        let preview = cfg!(any(
            feature = "cone-test-20s",
            feature = "fox-test-20s",
            feature = "bullfinch-test-20s",
            feature = "gift-test-20s",
            feature = "star-test-20s"
        ));
        let interval = frequency.saturating_mul(if preview { 20 } else { 180 }).max(1);
        Self { next: now.saturating_add(interval), interval }
    }

    pub fn due(&self, now: i64) -> bool {
        now >= self.next
    }

    /// Consume at most one draw, even after sleep or a long pause.
    pub fn poll(&mut self, now: i64, random: u64, trees: usize, same_species: usize, gifts: usize, legendary: usize) -> Option<Event> {
        if now < self.next {
            return None;
        }
        self.next = now.saturating_add(self.interval);
        if cfg!(feature = "cone-test-20s") {
            return Some(Event::Cone);
        }
        if cfg!(feature = "fox-test-20s") {
            return Some(Event::Fox);
        }
        if cfg!(feature = "bullfinch-test-20s") {
            return Some(Event::Bullfinch);
        }
        if cfg!(feature = "gift-test-20s") {
            return Some(Event::Gift);
        }
        if cfg!(feature = "star-test-20s") {
            return Some(Event::Star);
        }
        draw(random, trees, same_species, gifts, legendary)
    }
}

fn draw(random: u64, trees: usize, same_species: usize, gifts: usize, legendary: usize) -> Option<Event> {
    let value = random % TOTAL;
    let mut start = 0;
    for (event, weight) in WEIGHTS {
        if value < start + weight {
            // More trees reduce cone chances, including the old species cap.
            let adjusted = if event == Event::Cone {
                let remaining = 4_usize.saturating_sub(same_species) as u64;
                let scale = 10_u64.saturating_add(trees as u64);
                weight * remaining * 10 / (4 * scale)
            } else if event == Event::Gift {
                weight * 10 / 10_u64.saturating_add(gifts as u64)
            } else if event == Event::Star {
                weight * 10_usize.saturating_sub(legendary).min(10) as u64 / 10
            } else {
                weight
            };
            return (value - start < adjusted).then_some(event);
        }
        start += weight;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "bullfinch-test-20s", not(any(feature = "cone-test-20s", feature = "fox-test-20s"))))]
    #[test]
    fn bullfinch_preview_forces_the_shared_draw_every_twenty_seconds() {
        let mut schedule = Schedule::new(0, 1);
        assert_eq!(schedule.poll(19, 0, 0, 0, 0, 0), None);
        assert_eq!(schedule.poll(20, 0, 0, 0, 0, 0), Some(Event::Bullfinch));
        assert_eq!(schedule.poll(39, 0, 0, 0, 0, 0), None);
        assert_eq!(schedule.poll(40, 0, 0, 0, 0, 0), Some(Event::Bullfinch));
    }

    #[test]
    fn exactly_one_event_or_none_and_expected_weights() {
        assert!(WEIGHTS.iter().map(|(_, weight)| weight).sum::<u64>() < TOTAL);
        for (event, weight) in WEIGHTS {
            assert_eq!((0..TOTAL).filter(|&value| draw(value, 0, 0, 0, 0) == Some(event)).count(), weight as usize);
        }
        assert_eq!((0..TOTAL).filter(|&value| draw(value, 0, 0, 0, 0) == Some(Event::Truck)).count(), 1080);
        assert_eq!((0..TOTAL).filter(|&value| draw(value, 0, 0, 0, 0) == Some(Event::RoadTrain)).count(), 360);
        assert!(draw(TOTAL - 1, 0, 0, 0, 0).is_none());
        assert_eq!((0..TOTAL).filter(|&value| draw(value, 4, 4, 0, 0) == Some(Event::Cone)).count(), 0);
        assert!((0..TOTAL).filter(|&value| draw(value, 0, 0, 2, 0) == Some(Event::Gift)).count() < 348);
        assert_eq!((0..TOTAL).filter(|&value| draw(value, 0, 0, 0, 1) == Some(Event::Star)).count(), 1173);
        assert_eq!((0..TOTAL).filter(|&value| draw(value, 0, 0, 0, 2) == Some(Event::Star)).count(), 1043);
    }

    #[test]
    fn pause_does_not_cause_a_burst_of_draws() {
        let mut schedule = Schedule::new(0, 1);
        assert!(schedule.poll(9, 0, 0, 0, 0, 0).is_none());
        assert!(schedule.poll(1000, 0, 0, 0, 0, 0).is_some());
        assert!(schedule.poll(1000, 0, 0, 0, 0, 0).is_none());
    }

    #[test]
    fn regular_interval_is_three_minutes() {
        if cfg!(any(
            feature = "cone-test-20s",
            feature = "fox-test-20s",
            feature = "bullfinch-test-20s",
            feature = "gift-test-20s",
            feature = "star-test-20s"
        )) {
            return;
        }
        let mut schedule = Schedule::new(100, 10);
        assert!(!schedule.due(1899));
        assert!(schedule.due(1900));
        assert_eq!(schedule.poll(1900, 0, 0, 0, 0, 0), Some(Event::Cone));
        assert!(!schedule.due(3699));
        assert!(schedule.due(3700));
    }
}
