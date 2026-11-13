//! Coordinate the vehicles across displays and keep their schedules independent.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vehicle {
    Plow,
    Truck,
}

impl Vehicle {
    fn index(self) -> usize {
        match self {
            Self::Plow => 0,
            Self::Truck => 1,
        }
    }
    fn other(self) -> Self {
        match self {
            Self::Plow => Self::Truck,
            Self::Truck => Self::Plow,
        }
    }
}

#[derive(Clone, Copy)]
struct Schedule {
    interval: u32,
    next: i64,
}

pub struct Traffic {
    schedules: [Schedule; 2],
    active: Option<Vehicle>,
    last: Option<Vehicle>,
    ready_at: i64,
}

impl Traffic {
    pub fn new(plow: u32, truck: u32, now: i64, frequency: i64) -> Self {
        let schedule = |interval| Schedule { interval, next: now + i64::from(interval) * 60 * frequency };
        Self { schedules: [schedule(plow), schedule(truck)], active: None, last: None, ready_at: now }
    }

    /// Selecting a nonzero interval requests an immediate pass, subject to the other vehicle and the gap.
    pub fn select(&mut self, vehicle: Vehicle, interval: u32, now: i64) {
        self.schedules[vehicle.index()] = Schedule { interval, next: now };
    }

    /// A lottery vehicle consumes its draw when traffic is unavailable.
    pub fn start_unscheduled(&mut self, vehicle: Vehicle, now: i64, allowed: bool, busy: bool) -> bool {
        if !allowed || busy || self.active.is_some() || (self.last.is_some_and(|last| last != vehicle) && now < self.ready_at) {
            return false;
        }
        self.active = Some(vehicle);
        self.last = Some(vehicle);
        true
    }

    /// `busy` remains true until the vehicle has exited its selected monitor.
    pub fn poll(&mut self, now: i64, frequency: i64, allowed: bool, busy: bool) -> Option<Vehicle> {
        if self.active.is_some() && !busy {
            self.active = None;
            self.ready_at = now + 90 * frequency;
        }
        if busy || self.active.is_some() || !allowed {
            return None;
        }
        let due = |vehicle: Vehicle| {
            let schedule = self.schedules[vehicle.index()];
            schedule.interval != 0 && now >= schedule.next
        };
        let vehicle = if let Some(last) = self.last {
            // Give a waiting different vehicle the next turn, without delaying the sole enabled vehicle.
            let other = last.other();
            if due(other) {
                if now < self.ready_at {
                    return None;
                }
                other
            } else if due(last) {
                last
            } else {
                return None;
            }
        } else if due(Vehicle::Plow) {
            Vehicle::Plow
        } else if due(Vehicle::Truck) {
            Vehicle::Truck
        } else {
            return None;
        };
        let schedule = &mut self.schedules[vehicle.index()];
        schedule.next = now + i64::from(schedule.interval) * 60 * frequency;
        self.active = Some(vehicle);
        self.last = Some(vehicle);
        Some(vehicle)
    }
}

/// Select exactly one eligible monitor from an independently sampled random value.
pub fn choose_monitor(eligible: &[bool], random: u64) -> Option<usize> {
    let count = eligible.iter().filter(|&&available| available).count();
    if count == 0 {
        return None;
    }
    let selected = (random % count as u64) as usize;
    eligible.iter().enumerate().filter(|(_, available)| **available).nth(selected).map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lottery_truck_obeys_busy_and_inter_vehicle_gap() {
        let mut traffic = Traffic::new(0, 0, 0, 1);
        traffic.select(Vehicle::Plow, 1, 0);
        assert_eq!(traffic.poll(0, 1, true, false), Some(Vehicle::Plow));
        assert!(!traffic.start_unscheduled(Vehicle::Truck, 1, true, true));
        assert_eq!(traffic.poll(10, 1, true, false), None);
        assert!(!traffic.start_unscheduled(Vehicle::Truck, 99, true, false));
        assert!(!traffic.start_unscheduled(Vehicle::Truck, 100, false, false));
        assert!(traffic.start_unscheduled(Vehicle::Truck, 100, true, false));
        assert_eq!(traffic.poll(101, 1, true, true), None);
        assert_eq!(traffic.poll(110, 1, true, false), None);
        assert_eq!(traffic.poll(199, 1, true, false), None);
        assert_eq!(traffic.poll(200, 1, true, false), Some(Vehicle::Plow));
    }

    #[test]
    fn simultaneous_departures_wait_until_90_seconds_after_the_last_exit() {
        let mut traffic = Traffic::new(0, 0, 0, 1);
        traffic.select(Vehicle::Plow, 1, 10);
        traffic.select(Vehicle::Truck, 3, 10);
        assert_eq!(traffic.poll(10, 1, true, false), Some(Vehicle::Plow));
        assert_eq!(traffic.poll(30, 1, true, true), None);
        assert_eq!(traffic.poll(40, 1, true, false), None);
        assert_eq!(traffic.poll(129, 1, true, false), None);
        assert_eq!(traffic.poll(130, 1, true, false), Some(Vehicle::Truck));
        assert_eq!(traffic.schedules[Vehicle::Truck.index()].next, 310);
        assert_eq!(traffic.poll(145, 1, true, false), None);
        assert_eq!(traffic.poll(235, 1, true, false), Some(Vehicle::Plow));
    }

    #[test]
    fn selection_and_disabling_do_not_skip_the_cooldown_or_start_in_other_modes() {
        let mut traffic = Traffic::new(0, 0, 0, 10);
        traffic.select(Vehicle::Truck, 1, 100);
        assert_eq!(traffic.poll(100, 10, false, false), None);
        assert_eq!(traffic.poll(110, 10, true, false), Some(Vehicle::Truck));
        traffic.select(Vehicle::Plow, 5, 120);
        assert_eq!(traffic.poll(200, 10, true, true), None);
        assert_eq!(traffic.poll(300, 10, true, false), None);
        traffic.select(Vehicle::Plow, 0, 400);
        assert_eq!(traffic.poll(1200, 10, true, false), Some(Vehicle::Truck));
    }

    #[test]
    fn a_single_vehicle_keeps_its_interval_without_a_forced_gap() {
        let mut traffic = Traffic::new(0, 0, 0, 1);
        traffic.select(Vehicle::Plow, 1, 0);
        assert_eq!(traffic.poll(0, 1, true, false), Some(Vehicle::Plow));
        assert_eq!(traffic.poll(20, 1, true, false), None);
        assert_eq!(traffic.poll(59, 1, true, false), None);
        assert_eq!(traffic.poll(60, 1, true, false), Some(Vehicle::Plow));
    }

    #[test]
    fn selects_only_one_eligible_monitor_and_can_choose_each_available_monitor() {
        assert_eq!(choose_monitor(&[], 0), None);
        assert_eq!(choose_monitor(&[false, false], 1), None);
        assert_eq!(choose_monitor(&[false, true, false], u64::MAX), Some(1));
        let chosen: std::collections::HashSet<_> = (0..12).filter_map(|random| choose_monitor(&[true, false, true, true], random)).collect();
        assert_eq!(chosen, [0, 2, 3].into_iter().collect());
    }
}
