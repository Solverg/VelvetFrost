//! The shapes made by rolling and stacking snow, independent of input and rendering.
//!
//! Coordinates use the simulation's convention: y increases upwards. A platform supplies
//! the current supporting edges and translates pointer events into `pick_up`, `drag` and
//! `release`; it is responsible for persisting completed changes.

use crate::geometry::V2;
use crate::simulation::WindowId;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_STACKS: usize = 64;
pub const MAX_BALLS_PER_STACK: usize = 16;
const MIN_RADIUS: f32 = 5.0;
const MAX_RADIUS: f32 = 36.0;
const GRAVITY: f32 = 900.0;
// Fall height is proportional to impact speed squared: sqrt(2) and sqrt(3)
// double and triple the former 360 px/s threshold's fall height.
const SNOWMAN_BREAK_SPEED: f32 = 360.0 * std::f32::consts::SQRT_2;
pub const OBJECT_BREAK_SPEED: f32 = 360.0 * 1.732_050_8;
const BALL_OVERLAP: f32 = 0.3;

/// A horizontal support. Ground uses id 0; window ids are only valid for the current session.
#[derive(Clone, Copy, Debug)]
pub struct Support {
    pub id: WindowId,
    pub left: f32,
    pub right: f32,
    pub top: f32,
}

/// A vertical stack; `base` is the centre of the bottom ball's underside.
#[derive(Clone, Debug, PartialEq)]
pub struct Stack {
    pub base: V2,
    /// Radii in bottom-to-top order. Three balls make a snowman.
    pub radii: Vec<f32>,
    /// Persistent visual identity; it does not affect physics.
    pub appearance: u64,
    /// Window-relative x while resting, or `None` while being carried or falling.
    pub support: Option<(WindowId, f32)>,
    velocity: V2,
}

impl Stack {
    pub fn height(&self) -> f32 {
        self.ball_centres().last().map_or(0.0, |(centre, radius)| centre.y + radius - self.base.y)
    }

    /// Height measured from zero, independent of the stack's position and its floating-point rounding.
    pub fn height_from_radii(&self) -> f32 {
        let mut y = 0.0;
        let mut previous: Option<f32> = None;
        for &radius in &self.radii {
            if let Some(previous) = previous {
                y -= previous.min(radius) * BALL_OVERLAP;
            }
            y += radius;
            y += radius;
            previous = Some(radius);
        }
        y
    }

    pub fn snowman(&self) -> bool {
        self.radii.len() >= 3
    }

    pub fn ball_centres(&self) -> impl Iterator<Item = (V2, f32)> + '_ {
        let mut y = self.base.y;
        let mut previous: Option<f32> = None;
        self.radii.iter().copied().map(move |radius| {
            if let Some(previous) = previous {
                y -= previous.min(radius) * BALL_OVERLAP;
            }
            y += radius;
            let centre = V2::new(self.base.x, y);
            y += radius;
            previous = Some(radius);
            (centre, radius)
        })
    }
}

#[derive(Default)]
pub struct Snowmen {
    stacks: Vec<Stack>,
    held: Option<(usize, V2)>,
}

impl Snowmen {
    pub fn stacks(&self) -> &[Stack] {
        &self.stacks
    }

    /// Remove a complete, unheld object for storage by its platform owner.
    pub fn take_resting(&mut self, appearance: u64) -> Option<Stack> {
        if self.held.is_some() {
            return None;
        }
        let index = self.stacks.iter().position(|stack| stack.appearance == appearance && stack.support.is_some())?;
        Some(self.stacks.remove(index))
    }

    pub fn can_create(&self) -> bool {
        self.held.is_none() && self.stacks.len() < MAX_STACKS
    }

    /// Restore only validated objects; transient velocity and active drags never survive a restart.
    pub fn restore(&mut self, base: V2, radii: Vec<f32>, support: Option<(WindowId, f32)>) -> bool {
        self.restore_with_appearance(base, radii, support, new_appearance())
    }

    pub fn restore_with_appearance(&mut self, base: V2, radii: Vec<f32>, support: Option<(WindowId, f32)>, appearance: u64) -> bool {
        if self.stacks.len() >= MAX_STACKS
            || !base.x.is_finite()
            || !base.y.is_finite()
            || radii.is_empty()
            || radii.len() > MAX_BALLS_PER_STACK
            || radii.iter().any(|r| !r.is_finite() || !(MIN_RADIUS..=MAX_RADIUS).contains(r))
            || support.is_some_and(|(_, x)| !x.is_finite())
        {
            return false;
        }
        self.stacks.push(Stack { base, radii, support, appearance, velocity: V2::ZERO });
        true
    }

    /// Start a new ball where snow was collected. The caller removes the corresponding snow.
    pub fn create(&mut self, centre: V2, radius: f32) -> bool {
        self.create_at(centre, centre, radius)
    }

    /// Begin a ball above the surface while keeping the grip under the pointer.
    pub fn create_at(&mut self, pointer: V2, centre: V2, radius: f32) -> bool {
        if self.held.is_some() || !radius.is_finite() || !centre.x.is_finite() || !centre.y.is_finite() {
            return false;
        }
        if !pointer.x.is_finite() || !pointer.y.is_finite() {
            return false;
        }
        let radius = radius.clamp(MIN_RADIUS, MAX_RADIUS);
        if !self.restore(V2::new(centre.x, centre.y - radius), vec![radius], None) {
            return false;
        }
        self.held = Some((self.stacks.len() - 1, self.stacks.last().unwrap().base - pointer));
        true
    }

    pub fn contains(&self, pointer: V2) -> bool {
        self.stacks.iter().any(|stack| stack.ball_centres().any(|(centre, radius)| (centre - pointer).length_squared() <= radius * radius))
    }

    pub fn held_stack(&self) -> Option<&Stack> {
        self.held.map(|(index, _)| &self.stacks[index])
    }

    /// Return the held object to its original support without making it fall.
    pub fn restore_held(&mut self, base: V2, support: (WindowId, f32)) -> bool {
        if !base.x.is_finite() || !base.y.is_finite() || !support.1.is_finite() {
            return false;
        }
        let Some((index, _)) = self.held.take() else { return false };
        let stack = &mut self.stacks[index];
        stack.base = base;
        stack.support = Some(support);
        stack.velocity = V2::ZERO;
        true
    }

    /// Reposition a resting object after a platform has resolved its placement.
    pub fn relocate_resting(&mut self, appearance: u64, base: V2, support: (WindowId, f32)) -> bool {
        if !base.x.is_finite() || !base.y.is_finite() || !support.1.is_finite() {
            return false;
        }
        let Some(stack) = self.stacks.iter_mut().find(|stack| stack.appearance == appearance && stack.support.is_some()) else { return false };
        stack.base = base;
        stack.support = Some(support);
        stack.velocity = V2::ZERO;
        true
    }

    pub fn moving(&self) -> bool {
        self.held.is_some() || self.stacks.iter().any(|stack| stack.support.is_none())
    }

    /// Pick up a ball from an unfinished stack, or a completed snowman as a whole.
    pub fn pick_up(&mut self, pointer: V2) -> bool {
        self.pick_up_where(pointer, |_| true)
    }

    /// Pick up a complete object selected by the platform's own hit testing.
    /// Unlike ball picking this never splits a stack, and keeps the actual pointer offset.
    pub fn pick_up_whole_where(&mut self, pointer: V2, hit: impl Fn(&Stack) -> bool) -> bool {
        if self.held.is_some() || !pointer.x.is_finite() || !pointer.y.is_finite() {
            return false;
        }
        let Some(index) = self.stacks.iter().rposition(hit) else { return false };
        let stack = &mut self.stacks[index];
        stack.support = None;
        stack.velocity = V2::ZERO;
        self.held = Some((index, stack.base - pointer));
        true
    }

    /// Pick only visible objects, as determined by the platform's window stacking order.
    pub fn pick_up_where(&mut self, pointer: V2, visible: impl Fn(&Stack) -> bool) -> bool {
        if self.held.is_some() {
            return false;
        }
        let Some(index) = self
            .stacks
            .iter()
            .rposition(|stack| visible(stack) && stack.ball_centres().any(|(centre, radius)| (centre - pointer).length_squared() <= radius * radius))
        else {
            return false;
        };
        if self.stacks[index].radii.len() == 2 && self.stacks.len() < MAX_STACKS {
            let ball = self.stacks[index]
                .ball_centres()
                .enumerate()
                .filter(|(_, (centre, radius))| (*centre - pointer).length_squared() <= radius * radius)
                .last();
            if let Some((ball_index, (centre, radius))) = ball {
                let upper_base = self.stacks[index].ball_centres().nth(1).map(|(centre, radius)| centre.y - radius);
                let stack = &mut self.stacks[index];
                stack.radii.remove(ball_index);
                if ball_index == 0 {
                    // The ball above loses its support and drops from its previous position.
                    stack.base.y = upper_base.unwrap();
                    stack.support = None;
                    stack.velocity = V2::ZERO;
                }
                let base = V2::new(centre.x, centre.y - radius);
                self.stacks.push(Stack { base, radii: vec![radius], support: None, appearance: new_appearance(), velocity: V2::ZERO });
                self.held = Some((self.stacks.len() - 1, base - pointer));
                return true;
            }
        }
        let stack = &mut self.stacks[index];
        stack.support = None;
        stack.velocity = V2::ZERO;
        self.held = Some((index, stack.base - pointer));
        true
    }

    pub fn holding(&self) -> bool {
        self.held.is_some()
    }

    /// Maximum snow area the currently held ball can still take up.
    pub fn growth_capacity(&self) -> f32 {
        let Some((index, _)) = self.held else { return 0.0 };
        let stack = &self.stacks[index];
        if stack.radii.len() != 1 {
            return 0.0;
        }
        std::f32::consts::PI * (MAX_RADIUS * MAX_RADIUS - stack.radii[0] * stack.radii[0])
    }

    pub fn drag(&mut self, pointer: V2) {
        if let Some((index, offset)) = self.held {
            if pointer.x.is_finite() && pointer.y.is_finite() {
                self.stacks[index].base = pointer + offset;
            }
        }
    }

    /// Adds collected snow area to a single held ball, preserving its bottom position.
    pub fn grow(&mut self, area: f32) {
        let Some((index, _)) = self.held else { return };
        let stack = &mut self.stacks[index];
        if stack.radii.len() != 1 || !area.is_finite() || area <= 0.0 {
            return;
        }
        let old = stack.radii[0];
        stack.radii[0] = (old * old + area / std::f32::consts::PI).sqrt().min(MAX_RADIUS);
    }

    /// Snap a single ball to a stack if placed close to its top; otherwise let it fall.
    pub fn release(&mut self, velocity: V2) {
        let Some((index, _)) = self.held.take() else { return };
        let held = &self.stacks[index];
        let target = if held.radii.len() == 1 {
            self.stacks
                .iter()
                .enumerate()
                .filter(|(i, stack)| {
                    let top = stack.base.y + stack.height();
                    let top_radius = *stack.radii.last().unwrap();
                    *i != index
                        && stack.radii.len() < MAX_BALLS_PER_STACK
                        && stack.support.is_some()
                        && (stack.base.x - held.base.x).abs() <= (top_radius + held.radii[0]) * 0.65
                        && held.base.y <= top + held.radii[0] * 0.75 + 5.0
                        && held.base.y + held.radii[0] >= top - top_radius * 0.5
                })
                .min_by(|(_, a), (_, b)| {
                    (held.base.y - (a.base.y + a.height()))
                        .abs()
                        .total_cmp(&(held.base.y - (b.base.y + b.height())).abs())
                        .then_with(|| (a.base.x - held.base.x).abs().total_cmp(&(b.base.x - held.base.x).abs()))
                })
                .map(|(i, _)| i)
        } else {
            None
        };
        if let Some(target) = target {
            let radius = self.stacks[index].radii[0];
            self.stacks[target].radii.push(radius);
            self.stacks.remove(index);
        } else {
            self.stacks[index].velocity = if velocity.x.is_finite() && velocity.y.is_finite() { velocity } else { V2::ZERO };
        }
    }

    /// A closed support is different from a temporarily hidden one: its stacks start falling.
    pub fn detach_support(&mut self, id: WindowId) {
        for stack in &mut self.stacks {
            if stack.support.is_some_and(|(support, _)| support == id) {
                stack.support = None;
                stack.velocity = V2::ZERO;
            }
        }
    }

    /// Follow supporting windows and advance free fall. Returns positions of broken stacks.
    /// Missing window supports are left in place so a temporarily hidden window can return.
    pub fn step(&mut self, dt: f32, supports: &[Support]) -> Vec<V2> {
        self.step_with_surface(dt, supports, |_, _| None)
    }

    /// Advance using the current snow height above each physical support, when available.
    pub fn step_with_surface(&mut self, dt: f32, supports: &[Support], surface_at: impl Fn(WindowId, f32) -> Option<f32>) -> Vec<V2> {
        self.step_with_surface_and_object_breakage(dt, supports, surface_at, false)
    }

    /// Use the object threshold for every stack when this collection represents trees.
    pub fn step_with_surface_and_object_breakage(
        &mut self,
        dt: f32,
        supports: &[Support],
        surface_at: impl Fn(WindowId, f32) -> Option<f32>,
        objects_only: bool,
    ) -> Vec<V2> {
        if !dt.is_finite() || dt <= 0.0 {
            return Vec::new();
        }
        let dt = dt.min(0.05);
        let mut broken = Vec::new();
        let mut index = 0;
        while index < self.stacks.len() {
            if self.held.is_some_and(|(held, _)| held == index) {
                index += 1;
                continue;
            }
            // Only resting, visible stacks can support another ball. Evaluate their current
            // window/snow height before stepping the falling ball, regardless of vector order.
            let targets: Vec<_> = if self.stacks[index].support.is_none() && self.stacks[index].radii.len() == 1 {
                self.stacks
                    .iter()
                    .enumerate()
                    .filter_map(|(other, stack)| {
                        if other == index || stack.radii.len() >= MAX_BALLS_PER_STACK {
                            return None;
                        }
                        let (id, offset) = stack.support?;
                        let support = supports
                            .iter()
                            .find(|s| s.id == id && s.left.is_finite() && s.right.is_finite() && s.top.is_finite() && s.left <= s.right)?;
                        let x = (support.left + offset).clamp(support.left, support.right);
                        let base = surface_at(id, x).filter(|height| height.is_finite()).unwrap_or(support.top).max(support.top);
                        Some((other, x, base + stack.height(), *stack.radii.last().unwrap()))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let stack = &mut self.stacks[index];
            let break_speed = if !objects_only && stack.snowman() { SNOWMAN_BREAK_SPEED } else { OBJECT_BREAK_SPEED };
            if let Some((id, offset)) = stack.support {
                if let Some(surface) =
                    supports.iter().find(|s| s.id == id && s.left.is_finite() && s.right.is_finite() && s.top.is_finite() && s.left <= s.right)
                {
                    let x = (surface.left + offset).clamp(surface.left, surface.right);
                    let top = surface_at(id, x).filter(|height| height.is_finite()).unwrap_or(surface.top).max(surface.top);
                    stack.base = V2::new(x, top);
                }
            } else {
                if let Some(ground) = supports.iter().find(|surface| {
                    surface.id == 0
                        && surface.left.is_finite()
                        && surface.right.is_finite()
                        && surface.top.is_finite()
                        && surface.left <= surface.right
                }) {
                    stack.base.x = stack.base.x.clamp(ground.left, ground.right);
                    stack.base.y = stack.base.y.max(ground.top);
                }
                let previous = stack.base;
                stack.velocity.y -= GRAVITY * dt;
                stack.base += stack.velocity * dt;
                if let Some(ground) = supports
                    .iter()
                    .find(|surface| surface.id == 0 && surface.left.is_finite() && surface.right.is_finite() && surface.left <= surface.right)
                {
                    let x = stack.base.x.clamp(ground.left, ground.right);
                    if x != stack.base.x {
                        stack.velocity.x = 0.0;
                        stack.base.x = x;
                    }
                }
                let landing = supports
                    .iter()
                    .filter_map(|surface| {
                        let top = surface_at(surface.id, stack.base.x).filter(|height| height.is_finite()).unwrap_or(surface.top).max(surface.top);
                        let hit = surface.left.is_finite()
                            && surface.right.is_finite()
                            && surface.top.is_finite()
                            && surface.left <= surface.right
                            && stack.base.x >= surface.left
                            && stack.base.x <= surface.right
                            && previous.y >= surface.top
                            && stack.velocity.y <= 0.0
                            && stack.base.y <= top;
                        hit.then_some((surface, top))
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1));
                let stack_landing = if stack.radii.len() == 1 && stack.velocity.y <= 0.0 {
                    targets
                        .iter()
                        .filter(|(_, x, top, radius)| {
                            previous.y >= *top && stack.base.y <= *top && (stack.base.x - *x).abs() <= (*radius + stack.radii[0]) * 0.65
                        })
                        .max_by(|a, b| a.2.total_cmp(&b.2))
                        .copied()
                } else {
                    None
                };
                if let Some((target, _, top, _)) = stack_landing.filter(|candidate| landing.is_none_or(|surface| candidate.2 >= surface.1)) {
                    stack.base.y = top;
                    if -stack.velocity.y >= break_speed {
                        broken.push(stack.base);
                    } else {
                        let radius = stack.radii[0];
                        self.stacks[target].radii.push(radius);
                    }
                    self.stacks.remove(index);
                    if let Some((held, offset)) = self.held
                        && held > index
                    {
                        self.held = Some((held - 1, offset));
                    }
                    continue;
                }
                if let Some((surface, top)) = landing {
                    stack.base.y = top;
                    if -stack.velocity.y >= break_speed {
                        broken.push(stack.base);
                        self.stacks.remove(index);
                        if let Some((held, offset)) = self.held
                            && held > index
                        {
                            self.held = Some((held - 1, offset));
                        }
                        continue;
                    }
                    stack.support = Some((surface.id, stack.base.x - surface.left));
                    stack.velocity = V2::ZERO;
                }
            }
            index += 1;
        }
        broken
    }
}

fn new_appearance() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |t| t.as_nanos() as u64);
    let mut x = time ^ NEXT.fetch_add(0x9e3779b97f4a7c15, Ordering::Relaxed);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUND: Support = Support { id: 0, left: 0.0, right: 500.0, top: 0.0 };

    fn settle(snowmen: &mut Snowmen, supports: &[Support]) {
        for _ in 0..180 {
            snowmen.step(1.0 / 60.0, supports);
        }
    }

    #[test]
    fn held_object_can_return_to_its_original_support() {
        let mut objects = Snowmen::default();
        let base = V2::new(100.0, 25.0);
        assert!(objects.restore_with_appearance(base, vec![25.0; 3], Some((12, 30.0)), 7));
        assert!(objects.pick_up_whole_where(base, |stack| stack.appearance == 7));
        objects.drag(V2::new(300.0, 140.0));
        assert!(!objects.restore_held(V2::new(f32::NAN, 0.0), (12, 30.0)));
        assert!(objects.holding());
        assert!(objects.restore_held(base, (12, 30.0)));
        assert!(!objects.holding());
        assert_eq!(objects.stacks()[0].base, base);
        assert_eq!(objects.stacks()[0].support, Some((12, 30.0)));
    }

    #[test]
    fn balls_stack_and_a_snowman_can_be_picked_up_whole() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 0.0), vec![15.0, 10.0], Some((0, 100.0))));
        assert!(s.create(V2::new(102.0, 56.0), 6.0));
        s.release(V2::ZERO);
        assert!(s.stacks()[0].snowman());
        assert!(s.pick_up(V2::new(100.0, 56.0)));
        s.drag(V2::new(200.0, 76.0));
        s.release(V2::ZERO);
        settle(&mut s, &[GROUND]);
        assert!(s.stacks()[0].snowman());
        assert!((s.stacks()[0].base.x - 200.0).abs() < 0.01);
    }

    #[test]
    fn a_released_ball_lands_on_a_stack_instead_of_falling_through_it() {
        for radii in [vec![15.0], vec![15.0, 10.0], vec![15.0, 10.0, 6.0]] {
            let mut s = Snowmen::default();
            assert!(s.restore(V2::new(100.0, 0.0), radii.clone(), Some((0, 100.0))));
            let top = s.stacks()[0].height();
            assert!(s.create(V2::new(100.0, top + 31.0), 6.0));
            s.release(V2::ZERO);
            let mut broken = Vec::new();
            for _ in 0..180 {
                broken.extend(s.step(1.0 / 60.0, &[GROUND]));
            }
            assert!(broken.is_empty(), "a short fall onto {radii:?} must not break");
            assert_eq!(s.stacks().len(), 1);
            let mut expected = radii;
            expected.push(6.0);
            assert_eq!(s.stacks()[0].radii, expected);
        }
    }

    #[test]
    fn a_fast_ball_breaks_on_a_stack_instead_of_passing_through_it() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 0.0), vec![15.0], Some((0, 100.0))));
        assert!(s.create(V2::new(100.0, 300.0), 6.0));
        s.release(V2::ZERO);
        let mut broken = Vec::new();
        for _ in 0..180 {
            broken.extend(s.step(1.0 / 60.0, &[GROUND]));
        }
        assert_eq!(broken.len(), 1);
        assert!((broken[0].y - 30.0).abs() < 0.01);
        assert_eq!(s.stacks()[0].radii, vec![15.0]);
    }

    #[test]
    fn a_ball_released_overlapping_the_top_attaches() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 0.0), vec![15.0], Some((0, 100.0))));
        assert!(s.create(V2::new(100.0, 26.0), 6.0));
        s.release(V2::ZERO);
        assert_eq!(s.stacks().len(), 1);
        assert_eq!(s.stacks()[0].radii, vec![15.0, 6.0]);
    }

    #[test]
    fn falling_ball_stacks_on_a_window_and_preserves_another_held_ball() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(150.0, 150.0), vec![6.0], None));
        assert!(s.restore(V2::new(150.0, 100.0), vec![10.0], Some((7, 50.0))));
        assert!(s.create(V2::new(300.0, 100.0), 8.0));
        let window = Support { id: 7, left: 100.0, right: 250.0, top: 100.0 };
        for _ in 0..90 {
            s.step(1.0 / 60.0, &[GROUND, window]);
        }
        assert_eq!(s.stacks().len(), 2);
        assert_eq!(s.stacks()[0].radii, vec![10.0, 6.0]);
        assert_eq!(s.stacks()[0].support, Some((7, 50.0)));
        assert_eq!(s.held_stack().unwrap().radii, vec![8.0]);
    }

    #[test]
    fn ball_released_to_the_side_does_not_attach() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 0.0), vec![15.0], Some((0, 100.0))));
        assert!(s.create(V2::new(160.0, 60.0), 6.0));
        s.release(V2::ZERO);
        settle(&mut s, &[GROUND]);
        assert_eq!(s.stacks().len(), 2);
        assert_eq!(s.stacks()[0].radii, vec![15.0]);
        assert_eq!(s.stacks()[1].support, Some((0, 160.0)));
    }

    #[test]
    fn follows_window_and_a_hard_fall_breaks_it() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 100.0), vec![15.0, 10.0, 6.0], Some((7, 50.0))));
        s.step(0.016, &[Support { id: 7, left: 100.0, right: 300.0, top: 250.0 }]);
        assert_eq!(s.stacks()[0].base, V2::new(150.0, 250.0));
        assert!(s.pick_up(V2::new(150.0, 265.0)));
        s.release(V2::ZERO);
        let mut broken = Vec::new();
        for _ in 0..120 {
            broken.extend(s.step(1.0 / 60.0, &[GROUND]));
        }
        assert_eq!(broken.len(), 1);
        assert!(s.stacks().is_empty());
    }

    #[test]
    fn hard_falls_break_single_balls_and_incomplete_stacks_too() {
        for radii in [vec![8.0], vec![15.0, 10.0], vec![15.0, 10.0, 6.0]] {
            let mut s = Snowmen::default();
            assert!(s.restore(V2::new(100.0, 300.0), radii.clone(), None));
            let mut broken = Vec::new();
            for _ in 0..180 {
                broken.extend(s.step(1.0 / 60.0, &[GROUND]));
            }
            assert_eq!(broken.len(), 1, "hard fall should break {radii:?}");
            assert!(s.stacks().is_empty());
        }
    }

    #[test]
    fn short_falls_leave_balls_and_snowmen_intact() {
        for radii in [vec![8.0], vec![15.0, 10.0], vec![15.0, 10.0, 6.0]] {
            let mut s = Snowmen::default();
            assert!(s.restore(V2::new(100.0, 40.0), radii.clone(), None));
            let mut broken = Vec::new();
            for _ in 0..180 {
                broken.extend(s.step(1.0 / 60.0, &[GROUND]));
            }
            assert!(broken.is_empty(), "short fall should not break {radii:?}");
            assert_eq!(s.stacks()[0].support, Some((0, 100.0)));
        }
    }

    #[test]
    fn fall_height_thresholds_depend_on_object_type_and_landing_surface() {
        // The former 360 px/s threshold corresponds to 72 units at 900 px/s².
        for (radii, trees, height, should_break) in [
            (vec![15.0, 10.0, 6.0], false, 140.0, false),
            (vec![15.0, 10.0, 6.0], false, 148.0, true),
            (vec![8.0], false, 212.0, false),
            (vec![8.0], false, 220.0, true),
            (vec![15.0, 10.0], false, 212.0, false),
            (vec![15.0, 10.0, 6.0], true, 212.0, false),
            (vec![15.0, 10.0, 6.0], true, 220.0, true),
        ] {
            let mut s = Snowmen::default();
            assert!(s.restore(V2::new(100.0, height), radii.clone(), None));
            let mut broken = Vec::new();
            for _ in 0..1000 {
                broken.extend(s.step_with_surface_and_object_breakage(0.001, &[GROUND], |_, _| None, trees));
            }
            assert_eq!(!broken.is_empty(), should_break, "height {height}, radii {radii:?}, trees {trees}");
        }
        for (height, should_break) in [(212.0, false), (220.0, true)] {
            let mut s = Snowmen::default();
            assert!(s.restore(V2::new(100.0, 0.0), vec![15.0], Some((0, 100.0))));
            assert!(s.restore(V2::new(100.0, 30.0 + height), vec![6.0], None));
            let mut broken = Vec::new();
            for _ in 0..1000 {
                broken.extend(s.step(0.001, &[GROUND]));
            }
            assert_eq!(!broken.is_empty(), should_break, "ball landing on stack from {height}");
            assert_eq!(s.stacks()[0].radii.len(), if should_break { 1 } else { 2 });
        }
    }

    #[test]
    fn invalid_saved_data_is_rejected() {
        let mut s = Snowmen::default();
        assert!(!s.restore(V2::ZERO, vec![f32::NAN], None));
        assert!(!s.restore(V2::ZERO, vec![6.0; MAX_BALLS_PER_STACK + 1], None));
        assert!(!s.restore(V2::new(f32::INFINITY, 0.0), vec![6.0], None));
    }

    #[test]
    fn stacked_balls_overlap_and_height_matches_the_visible_top() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 20.0), vec![15.0, 10.0, 6.0], Some((0, 100.0))));
        let stack = &s.stacks()[0];
        let balls: Vec<_> = stack.ball_centres().collect();
        for pair in balls.windows(2) {
            let distance = pair[1].0.y - pair[0].0.y;
            assert!(distance < pair[0].1 + pair[1].1, "adjacent snowballs must overlap");
            assert!(distance > pair[0].1.max(pair[1].1), "both snowballs must stay distinct");
        }
        assert!((stack.base.y + stack.height() - (balls[2].0.y + balls[2].1)).abs() < 0.001);
        assert!((stack.height_from_radii() - stack.height()).abs() < 0.001);
        let height = stack.height_from_radii();
        let appearance = stack.appearance;
        assert!(s.relocate_resting(appearance, V2::new(100.0, 1024.37), (0, 100.0)));
        assert_eq!(s.stacks()[0].height_from_radii(), height);
    }

    #[test]
    fn picking_up_the_lower_ball_preserves_the_upper_balls_position() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 0.0), vec![15.0, 10.0], Some((0, 100.0))));
        let before = s.stacks()[0].ball_centres().nth(1).unwrap().0;
        assert!(s.pick_up(V2::new(100.0, 8.0)));
        assert_eq!(s.stacks()[0].ball_centres().next().unwrap().0, before);
        assert_eq!(s.held_stack().unwrap().radii, vec![15.0]);
    }

    #[test]
    fn picking_in_the_overlap_selects_the_visible_upper_ball() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 0.0), vec![15.0, 10.0], Some((0, 100.0))));
        assert!(s.pick_up(V2::new(100.0, 29.0)));
        assert_eq!(s.held_stack().unwrap().radii, vec![10.0]);
        assert_eq!(s.stacks()[0].radii, vec![15.0]);
    }

    #[test]
    fn hidden_windows_wait_but_closed_windows_release_their_stacks() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 50.0), vec![10.0], Some((7, 50.0))));
        settle(&mut s, &[GROUND]);
        assert_eq!(s.stacks()[0].base.y, 50.0);
        s.detach_support(7);
        settle(&mut s, &[GROUND]);
        assert_eq!(s.stacks()[0].base.y, 0.0);
        assert_eq!(s.stacks()[0].support, Some((0, 100.0)));
    }

    #[test]
    fn snow_height_supports_balls_and_tracks_changes_on_each_ledge() {
        let mut s = Snowmen::default();
        let window = Support { id: 7, left: 0.0, right: 500.0, top: 100.0 };
        assert!(s.restore(V2::new(100.0, 125.0), vec![8.0], None));
        assert!(s.restore(V2::new(300.0, 30.0), vec![8.0], None));
        let height = |id, x| {
            Some(if id == 7 {
                115.0
            } else if x < 200.0 {
                10.0
            } else {
                20.0
            })
        };
        for _ in 0..180 {
            s.step_with_surface(1.0 / 60.0, &[GROUND, window], height);
        }
        assert_eq!(s.stacks()[0].base.y, 115.0);
        assert_eq!(s.stacks()[0].support, Some((7, 100.0)));
        assert_eq!(s.stacks()[1].base.y, 20.0);
        assert_eq!(s.stacks()[1].support, Some((0, 300.0)));
        s.step_with_surface(0.016, &[GROUND, window], |id, _| Some(if id == 7 { 118.0 } else { 5.0 }));
        assert_eq!(s.stacks()[0].base.y, 118.0);
        assert_eq!(s.stacks()[1].base.y, 5.0);
    }

    #[test]
    fn balls_in_an_unfinished_stack_can_be_repositioned_individually() {
        let mut s = Snowmen::default();
        assert!(s.restore(V2::new(100.0, 0.0), vec![12.0, 8.0], Some((0, 100.0))));
        assert!(s.pick_up(V2::new(100.0, 32.0)));
        assert_eq!(s.stacks().len(), 2);
        assert_eq!(s.stacks()[0].radii, vec![12.0]);
        assert_eq!(s.held_stack().unwrap().radii, vec![8.0]);
        s.drag(V2::new(150.0, 32.0));
        s.release(V2::ZERO);
        settle(&mut s, &[GROUND]);
        assert_eq!(s.stacks().len(), 2);
        assert!((s.stacks()[1].base.x - 150.0).abs() < 0.01);
    }
}
