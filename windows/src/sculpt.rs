//! Screen-local snowball interaction. Input is accepted only by the overlay's explicit sculpting mode.
use std::collections::{HashMap, HashSet};
use std::f32::consts::PI;
use std::time::{SystemTime, UNIX_EPOCH};

use snowhush_core::geometry::V2;
use snowhush_core::simulation::{ScatterPiece, SnowSimulation, Sprite, WindowSurface};
use snowhush_core::snowmen::{MAX_BALLS_PER_STACK, Snowmen, Support};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, GetWindowThreadProcessId, IsWindow};

use crate::cone::Species;
use crate::{log, prefs, tracker};

const ICON_RADIUS: f32 = 14.0;
const START_RADIUS: f32 = 5.0;

#[derive(Clone, Copy)]
pub enum PointerEvent {
    Down(V2),
    Up(V2),
    Cancel,
    Exit,
}

pub struct SculptureDraw {
    pub sprites: Vec<Sprite>,
    pub covers: Vec<[f32; 4]>,
    pub control: bool,
}

pub struct Sculpt {
    pub objects: Snowmen,
    tree: bool,
    active: bool,
    icon: Option<V2>,
    hover: Option<(V2, f32)>,
    hovered_tree: Option<u64>,
    previous: Option<V2>,
    display: String,
    pending: Vec<Saved>,
    legacy_appearances: HashSet<u64>,
    widths: HashMap<isize, f32>,
    keys: HashMap<isize, String>,
    resolve_delay: f32,
    last_saved: String,
    save_delay: f32,
    writable: bool,
    size: (f32, f32),
    animation: f64,
    tree_stages: HashMap<u64, u8>,
    tree_births: HashMap<u64, f32>,
    tree_species: HashMap<u64, Species>,
    tree_variants: HashMap<u64, u8>,
}

#[derive(Clone, Debug, PartialEq)]
enum Anchor {
    Ground(f32),
    Window(String, f32),
    Falling(f32, f32),
    Stored(f32),
}

#[derive(Clone, Debug, PartialEq)]
struct Saved {
    anchor: Anchor,
    radii: Vec<f32>,
    appearance: u64,
    legacy: bool,
    species: Species,
    variant: u8,
}

impl Sculpt {
    pub fn new(display: &str, width: f32, height: f32) -> Self {
        let loaded = prefs::sculptures(display);
        let read_ok = loaded.is_ok();
        let raw = loaded.ok().flatten().unwrap_or_else(|| "snowhush-sculptures-1\n".to_string());
        let saved = decode(&raw);
        let writable = read_ok && saved.is_some();
        if !writable {
            log!("could not read sculptures on {display}; leaving saved data unchanged");
        }
        let mut objects = Snowmen::default();
        let mut pending = Vec::new();
        let legacy_appearances = saved.as_ref().into_iter().flatten().filter(|entry| entry.legacy).map(|entry| entry.appearance).collect();
        let tree_species = saved.as_ref().into_iter().flatten().map(|entry| (entry.appearance, entry.species)).collect();
        let tree_variants = saved.as_ref().into_iter().flatten().map(|entry| (entry.appearance, entry.variant)).collect();
        for entry in saved.unwrap_or_default() {
            match entry.anchor {
                Anchor::Ground(x) => {
                    objects.restore_with_appearance(V2::new(x * width, 0.0), entry.radii, Some((0, x * width)), entry.appearance);
                }
                Anchor::Falling(x, y) => {
                    objects.restore_with_appearance(V2::new(x * width, y * height), entry.radii, None, entry.appearance);
                }
                Anchor::Window(_, _) | Anchor::Stored(_) => pending.push(entry),
            }
        }
        Sculpt {
            objects,
            tree: false,
            active: false,
            icon: None,
            hover: None,
            hovered_tree: None,
            previous: None,
            display: display.to_string(),
            pending,
            legacy_appearances,
            widths: HashMap::new(),
            keys: HashMap::new(),
            resolve_delay: 0.0,
            last_saved: raw,
            save_delay: 0.0,
            writable,
            size: (width, height),
            animation: 0.0,
            tree_stages: HashMap::new(),
            tree_births: HashMap::new(),
            tree_species,
            tree_variants,
        }
    }

    /// Trees have their own collection and persistence, but use the same support and fall physics.
    pub fn trees(display: &str, width: f32, height: f32) -> Self {
        let mut garden = Self::new(&format!("trees:{display}"), width, height);
        garden.tree = true;
        garden
    }

    pub fn plant(&mut self, position: V2, support: (isize, f32), random: u64, species: Species) -> bool {
        self.plant_with_variant(position, support, random, species, crate::cone::tree_variant(random))
    }

    pub fn plant_legendary(&mut self, position: V2, support: (isize, f32), random: u64, variant: u8) -> bool {
        if !crate::star_event::LEGENDARY_VARIANTS.contains(&variant) || !self.can_plant_legendary() || self.has_legendary_tree(variant) {
            return false;
        }
        self.plant_with_variant(position, support, random, Species::Spruce, variant)
    }

    fn plant_with_variant(&mut self, position: V2, support: (isize, f32), random: u64, species: Species, variant: u8) -> bool {
        if !self.tree || self.objects.stacks().len() + self.pending.len() >= 64 {
            return false;
        }
        let (radius, count) =
            if crate::star_event::LEGENDARY_VARIANTS.contains(&variant) { (celestial_radius(random), 5) } else { (tree_radius(random), 3) };
        let identity = (now_seconds().min(u32::MAX as u64) << 32) | (random & 0xffff_ffff);
        let planted = self.objects.restore_with_appearance(position, vec![radius; count], Some(support), identity);
        if planted {
            self.save_delay = 0.0;
            self.tree_births.insert(identity, 0.0);
            self.tree_species.insert(identity, species);
            self.tree_variants.insert(identity, variant);
        }
        planted
    }

    pub fn persist_now(&mut self) {
        self.save(self.size.0, self.size.1);
    }

    pub fn stop(&mut self) {
        if self.objects.holding() {
            self.objects.release(V2::ZERO);
        }
        self.active = false;
        self.icon = None;
        self.hover = None;
        self.hovered_tree = None;
        self.previous = None;
        self.save(self.size.0, self.size.1);
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn has_icon(&self) -> bool {
        self.icon.is_some()
    }

    pub fn restore_held(&mut self, base: V2, support: (isize, f32)) -> bool {
        if !self.objects.restore_held(base, support) {
            return false;
        }
        self.previous = None;
        self.icon = None;
        self.hovered_tree = None;
        true
    }

    pub fn tree_style(&self, id: u64) -> (Species, u8) {
        (self.tree_species.get(&id).copied().unwrap_or_default(), self.tree_variants.get(&id).copied().unwrap_or_default())
    }

    pub fn has_legendary_tree(&self, variant: u8) -> bool {
        self.objects.stacks().iter().any(|tree| self.tree_style(tree.appearance).1 == variant)
            || self.pending.iter().any(|tree| tree.variant == variant)
    }

    pub fn can_plant_legendary(&self) -> bool {
        self.tree && self.writable && self.objects.stacks().len() + self.pending.len() < 64
    }

    /// Count visible young and adult trees, excluding planted cones before they sprout.
    pub fn tree_population(&self) -> [usize; 3] {
        let now = now_seconds();
        let mut counts = [0; 3];
        for tree in self.objects.stacks() {
            if tree_stage_at(tree.appearance, now) > 0 {
                counts[self.tree_style(tree.appearance).0.code() as usize] += 1;
            }
        }
        counts
    }

    pub fn knows_tree(&self, id: u64) -> bool {
        self.objects.stacks().iter().any(|tree| tree.appearance == id) || self.pending.iter().any(|tree| tree.appearance == id)
    }

    pub fn collection_trees(&self) -> Vec<(u64, Species, u8, bool, u32)> {
        let now = now_seconds();
        let mut result: Vec<_> = self
            .objects
            .stacks()
            .iter()
            .filter(|tree| tree.support.is_some())
            .map(|tree| {
                let (species, variant) = self.tree_style(tree.appearance);
                (tree.appearance, species, variant, false, collection_height_mm(tree.height_from_radii(), tree.appearance, now))
            })
            .collect();
        result.extend(
            self.pending.iter().filter(|tree| matches!(tree.anchor, Anchor::Stored(_))).map(|tree| {
                (tree.appearance, tree.species, tree.variant, true, collection_height_mm(radii_height(&tree.radii), tree.appearance, now))
            }),
        );
        result
    }

    /// Change one tree in its existing per-monitor save, preserving identity and growth time.
    pub fn stored_tree_size(&self, id: u64) -> Option<(f32, f32)> {
        let tree = self.pending.iter().find(|tree| tree.appearance == id && matches!(tree.anchor, Anchor::Stored(_)))?;
        let Anchor::Stored(x) = tree.anchor else { unreachable!() };
        Some((x * self.size.0, radii_height(&tree.radii)))
    }

    pub fn set_tree_stored(&mut self, id: u64, stored: bool, placement: Option<(V2, (isize, f32))>) -> bool {
        if !self.tree || !self.writable || self.objects.holding() {
            return false;
        }
        if stored {
            let Some(stack) = self.objects.take_resting(id) else { return false };
            let x = (stack.base.x / self.size.0.max(1.0)).clamp(0.0, 1.0);
            let (species, variant) = self.tree_style(id);
            self.pending.push(Saved {
                anchor: Anchor::Stored(x),
                radii: stack.radii.clone(),
                appearance: id,
                legacy: self.legacy_appearances.contains(&id),
                species,
                variant,
            });
            if !self.save(self.size.0, self.size.1) {
                self.pending.pop();
                self.objects.restore_with_appearance(stack.base, stack.radii, stack.support, id);
                return false;
            }
        } else {
            let Some(index) = self.pending.iter().position(|tree| tree.appearance == id && matches!(tree.anchor, Anchor::Stored(_))) else {
                return false;
            };
            let saved = self.pending.remove(index);
            let Anchor::Stored(x) = saved.anchor else { unreachable!() };
            let x = x * self.size.0;
            let (base, support) = placement.unwrap_or((V2::new(x, 0.0), (0, x)));
            if !self.objects.restore_with_appearance(base, saved.radii.clone(), Some(support), id) {
                self.pending.insert(index, saved);
                return false;
            }
            if !self.save(self.size.0, self.size.1) {
                self.objects.take_resting(id);
                self.pending.insert(index, saved);
                return false;
            }
        }
        true
    }

    pub fn icon_hit_area(&self) -> Option<(V2, f32)> {
        self.icon.map(|centre| (centre, ICON_RADIUS + 3.0))
    }

    pub fn captures(&self, pointer: V2) -> bool {
        if self.tree {
            return self.objects.holding() || self.icon.is_some_and(|icon| (pointer - icon).length() <= ICON_RADIUS + 3.0);
        }
        self.active
            || self.objects.holding()
            || self.objects.contains(pointer)
            || self.icon.is_some_and(|icon| (pointer - icon).length() <= ICON_RADIUS + 3.0)
    }

    pub fn hit_areas(&self, windows: &[WindowSurface]) -> Vec<(V2, f32)> {
        if self.tree {
            return self.icon_hit_area().into_iter().collect();
        }
        self.objects
            .stacks()
            .iter()
            .filter(|stack| !stack.support.is_some_and(|(id, _)| id != 0 && !windows.iter().any(|window| window.id == id)))
            .flat_map(|stack| stack.ball_centres())
            .collect()
    }

    pub fn frame(
        &mut self,
        simulation: &mut SnowSimulation,
        windows: &[WindowSurface],
        pointer: V2,
        dt: f32,
        events: &[PointerEvent],
        allowed: bool,
    ) -> bool {
        let mut changed = false;
        if !allowed {
            if self.active || self.icon.is_some() || self.objects.holding() {
                self.stop();
                changed = true;
            }
            return changed;
        }

        self.size = (simulation.width(), simulation.height());
        if dt.is_finite() && dt > 0.0 {
            self.animation += dt.min(0.05) as f64;
        }
        if self.tree {
            self.tree_species.retain(|id, _| {
                self.objects.stacks().iter().any(|stack| stack.appearance == *id) || self.pending.iter().any(|entry| entry.appearance == *id)
            });
            self.tree_variants.retain(|id, _| {
                self.objects.stacks().iter().any(|stack| stack.appearance == *id) || self.pending.iter().any(|entry| entry.appearance == *id)
            });
            if !self.tree_births.is_empty() {
                changed = true;
                for age in self.tree_births.values_mut() {
                    *age += dt.max(0.0).min(0.05);
                }
                self.tree_births.retain(|_, age| *age < 0.55);
            }
            let now = now_seconds();
            self.tree_stages.retain(|id, _| self.objects.stacks().iter().any(|stack| stack.appearance == *id));
            for stack in self.objects.stacks() {
                let stage = tree_stage_at(stack.appearance, now);
                changed |= self.tree_stages.insert(stack.appearance, stage) != Some(stage);
                changed |= stage == 2;
            }
        } else {
            changed |= self.objects.stacks().iter().any(|stack| stack.snowman());
        }

        let mut supports = Vec::with_capacity(windows.len() + 1);
        supports.push(Support { id: 0, left: 0.0, right: simulation.width(), top: 0.0 });
        supports.extend(windows.iter().map(|surface| Support {
            id: surface.id,
            left: surface.frame.min_x(),
            right: surface.frame.max_x(),
            top: surface.frame.max_y(),
        }));
        for window in windows {
            self.widths.insert(window.id, window.frame.width);
        }
        self.resolve_delay -= dt;
        if !self.pending.is_empty() && self.resolve_delay <= 0.0 {
            self.resolve_delay = 1.0;
            let identities: Vec<_> = windows.iter().filter_map(|window| window_key(window.id).map(|key| (window, key))).collect();
            let mut saved_kinds = HashMap::<String, usize>::new();
            for saved in &self.pending {
                if let Anchor::Window(key, _) = &saved.anchor {
                    *saved_kinds.entry(window_kind(key)).or_default() += 1;
                }
            }
            self.pending.retain(|saved| {
                let Anchor::Window(key, fraction) = &saved.anchor else { return true };
                let Some((window, identity)) = matching_window(key, saved_kinds.get(&window_kind(key)) == Some(&1), &identities) else { return true };
                self.keys.insert(window.id, identity.clone());
                let x = window.frame.min_x() + fraction * window.frame.width;
                if self.objects.restore_with_appearance(
                    V2::new(x, window.frame.max_y()),
                    saved.radii.clone(),
                    Some((window.id, x - window.frame.min_x())),
                    saved.appearance,
                ) {
                    changed = true;
                    false
                } else {
                    true
                }
            });
        }
        let closed: Vec<_> = self
            .objects
            .stacks()
            .iter()
            .filter_map(|stack| stack.support.map(|s| s.0))
            .filter(|&id| id != 0 && !supports.iter().any(|surface| surface.id == id) && !unsafe { IsWindow(Some(HWND(id as _))).as_bool() })
            .collect();
        for id in closed {
            self.objects.detach_support(id);
            self.keys.remove(&id);
            changed = true;
        }

        if self.tree && !self.objects.holding() {
            // Keep the handle visible while travelling from the crown to the button.
            let hovered = self.objects.stacks().iter().rev().find(|stack| {
                let visible = !stack.support.is_some_and(|(id, _)| id != 0 && !windows.iter().any(|window| window.id == id));
                let h = tree_height(stack);
                let handle = tree_handle(stack, self.size);
                let in_area = (pointer.x - stack.base.x).abs() <= (h * 0.40).max(18.0)
                    && pointer.y >= stack.base.y - 8.0
                    && pointer.y <= handle.y + ICON_RADIUS + 3.0;
                visible && in_area
            });
            let next = hovered.map(|stack| (stack.appearance, tree_handle(stack, self.size)));
            let next_id = next.map(|(id, _)| id);
            let next_icon = next.map(|(_, icon)| icon);
            changed |= self.hovered_tree != next_id || self.icon != next_icon;
            self.hovered_tree = next_id;
            self.icon = next_icon;
        }

        if !self.active && !self.tree {
            let near = nearby_snow(simulation, pointer, 22.0).is_some() || self.objects.contains(pointer);
            if self.icon.is_some_and(|icon| {
                (pointer - icon).length() > 54.0
                    && !self.objects.contains(pointer)
                    && !events.iter().any(|event| matches!(*event, PointerEvent::Down(at) if (at - icon).length() <= ICON_RADIUS + 3.0))
            }) {
                self.icon = None;
                self.hover = None;
                changed = true;
            }
            if self.icon.is_none() && near {
                let (anchor, elapsed) = self.hover.get_or_insert((pointer, 0.0));
                if (*anchor - pointer).length() > 8.0 {
                    *anchor = pointer;
                    *elapsed = 0.0;
                } else {
                    *elapsed += dt;
                }
                if *elapsed >= 0.35 {
                    let stack = self
                        .objects
                        .stacks()
                        .iter()
                        .rev()
                        .find(|stack| stack.ball_centres().any(|(centre, radius)| (centre - *anchor).length_squared() <= radius * radius));
                    let desired = if let Some(stack) = stack {
                        let top = stack.base.y + stack.height() + ICON_RADIUS + 4.0;
                        if top <= simulation.height() - ICON_RADIUS {
                            V2::new(stack.base.x, top)
                        } else {
                            let radius = stack.radii.iter().copied().fold(0.0, f32::max);
                            let offset = radius + ICON_RADIUS + 4.0;
                            let x =
                                if stack.base.x + offset <= simulation.width() - ICON_RADIUS { stack.base.x + offset } else { stack.base.x - offset };
                            V2::new(x, anchor.y)
                        }
                    } else {
                        V2::new(anchor.x, anchor.y + 26.0)
                    };
                    self.icon = Some(V2::new(
                        desired.x.clamp(ICON_RADIUS, simulation.width() - ICON_RADIUS),
                        desired.y.min(simulation.height() - ICON_RADIUS),
                    ));
                    changed = true;
                }
            } else if self.icon.is_none() && !near {
                self.hover = None;
            }
        }

        for event in events {
            match *event {
                PointerEvent::Exit => {
                    if self.active || self.objects.holding() {
                        self.stop();
                        changed = true;
                    }
                }
                PointerEvent::Down(at) => {
                    let hovered_tree = self.hovered_tree;
                    if self.tree
                        && self.icon.is_some_and(|icon| (at - icon).length() <= ICON_RADIUS + 3.0)
                        && self.objects.pick_up_whole_where(at, |stack| {
                            hovered_tree == Some(stack.appearance)
                                && !stack.support.is_some_and(|(id, _)| id != 0 && !windows.iter().any(|window| window.id == id))
                        })
                    {
                        self.previous = Some(at);
                        changed = true;
                    } else if !self.tree
                        && self
                            .objects
                            .pick_up_where(at, |stack| !stack.support.is_some_and(|(id, _)| id != 0 && !windows.iter().any(|window| window.id == id)))
                    {
                        self.previous = Some(at);
                        changed = true;
                    } else if !self.tree && self.icon.is_some_and(|icon| (at - icon).length() <= ICON_RADIUS + 3.0) {
                        if self.active && self.objects.holding() {
                            self.objects.release(V2::ZERO);
                        }
                        self.active = !self.active;
                        self.previous = None;
                        if !self.active {
                            self.icon = None;
                            self.hover = None;
                        }
                        changed = true;
                    } else if self.active {
                        if self.objects.can_create()
                            && self.objects.stacks().len() + self.pending.len() < 64
                            && let Some((id, surface)) = nearby_snow(simulation, at, 18.0)
                        {
                            let area = PI * START_RADIUS * START_RADIUS;
                            if simulation.snow_available(id, at.x, 22.0) >= area {
                                simulation.collect_snow(id, at.x, 22.0, area);
                                self.objects.create_at(at, V2::new(at.x, surface.y + START_RADIUS), START_RADIUS);
                                self.previous = Some(at);
                                changed = true;
                            }
                        }
                    }
                }
                PointerEvent::Up(at) => {
                    if self.objects.holding() {
                        self.objects.drag(V2::new(at.x.clamp(0.0, simulation.width()), at.y.clamp(0.0, simulation.height())));
                        self.objects.release(V2::ZERO);
                        self.previous = None;
                        self.save_delay = 0.0;
                        changed = true;
                    }
                }
                PointerEvent::Cancel => {
                    if self.objects.holding() {
                        self.objects.release(V2::ZERO);
                        changed = true;
                    }
                    self.previous = None;
                }
            }
        }

        if self.objects.holding() {
            if let Some(previous) = self.previous {
                let travel = pointer - previous;
                let pointer = V2::new(pointer.x.clamp(0.0, simulation.width()), pointer.y.clamp(0.0, simulation.height()));
                self.objects.drag(pointer);
                if self.tree
                    && let Some(stack) = self.objects.held_stack()
                {
                    self.icon = Some(tree_handle(stack, self.size));
                }
                if let Some(ball) = self.objects.held_stack().filter(|stack| stack.radii.len() == 1) {
                    let base = ball.base;
                    if let Some((id, surface)) = nearby_snow(simulation, base, 12.0)
                        && (base.y - surface.y).abs() < 12.0
                    {
                        let area = (travel.x.abs() * 5.0).min(self.objects.growth_capacity());
                        let gathered = simulation.collect_snow(id, base.x, 12.0, area);
                        self.objects.grow(gathered);
                    }
                }
                if travel.length_squared() > 0.01 {
                    changed = true;
                }
            }
            self.previous = Some(pointer);
        }
        if self.objects.moving() {
            changed = true;
        }
        let before_step: Vec<_> = self.objects.stacks().iter().map(|stack| (stack.base, stack.support)).collect();
        for position in self.objects.step_with_surface_and_object_breakage(dt, &supports, |id, x| simulation.snow_surface_on(id, x), self.tree) {
            let pieces: Vec<_> = (0..50)
                .map(|i| {
                    let t = i as f32 / 49.0;
                    ScatterPiece {
                        position: position + V2::new((t - 0.5) * 18.0, 4.0),
                        velocity: V2::new((t - 0.5) * 160.0, 60.0 + (i % 7) as f32 * 12.0),
                        color: [0.96, 0.98, 1.0],
                    }
                })
                .collect();
            simulation.scatter(&pieces);
            self.save_delay = 0.0;
            changed = true;
        }
        changed |= before_step.len() != self.objects.stacks().len()
            || before_step.iter().zip(self.objects.stacks()).any(|(before, stack)| *before != (stack.base, stack.support));
        self.save_delay -= dt;
        if !self.objects.holding() && self.save_delay <= 0.0 {
            self.save(simulation.width(), simulation.height());
            self.save_delay = 0.25;
        }
        changed
    }

    fn save(&mut self, width: f32, height: f32) -> bool {
        if !self.writable || width <= 0.0 || height <= 0.0 {
            return false;
        }
        let mut saved = self.pending.clone();
        for stack in self.objects.stacks() {
            let anchor = match stack.support {
                Some((0, x)) => Anchor::Ground((x / width).clamp(0.0, 1.0)),
                Some((id, x)) => {
                    let key = if let Some(key) = self.keys.get(&id) {
                        key.clone()
                    } else {
                        let Some(key) = window_key(id) else { return false };
                        self.keys.insert(id, key.clone());
                        key
                    };
                    let Some(&window_width) = self.widths.get(&id) else { return false };
                    Anchor::Window(key, (x / window_width.max(1.0)).clamp(0.0, 1.0))
                }
                None => Anchor::Falling((stack.base.x / width).clamp(0.0, 1.0), (stack.base.y / height).clamp(0.0, 1.0)),
            };
            saved.push(Saved {
                anchor,
                radii: stack.radii.clone(),
                appearance: stack.appearance,
                legacy: self.legacy_appearances.contains(&stack.appearance),
                species: self.tree_species.get(&stack.appearance).copied().unwrap_or_default(),
                variant: self.tree_variants.get(&stack.appearance).copied().unwrap_or_default(),
            });
        }
        let value = encode(&saved);
        if value != self.last_saved {
            if prefs::set_sculptures(&self.display, &value) {
                self.last_saved = value;
            } else {
                log!("could not save sculptures on {}", self.display);
                return false;
            }
        }
        true
    }

    pub fn sprites(&self, windows: &[WindowSurface], _simulation: &SnowSimulation) -> Vec<SculptureDraw> {
        let mut groups = Vec::new();
        for stack in self.objects.stacks() {
            if stack.support.is_some_and(|(id, _)| id != 0 && !windows.iter().any(|window| window.id == id)) {
                continue;
            }
            // The interactive overlay shows sculptures over windows, just like the held ball.
            // Ground snow is also drawn over windows on the normal foreground overlay.
            let covers = if self.active || self.objects.holding() {
                Vec::new()
            } else {
                match stack.support {
                    Some((id, _)) if id != 0 => windows
                        .iter()
                        .find(|window| window.id == id)
                        .map(|window| window.cover.iter().map(|r| [r.min_x(), r.min_y(), r.max_x(), r.max_y()]).collect())
                        .unwrap_or_default(),
                    _ => Vec::new(),
                }
            };
            let mut sprites = if self.tree {
                tree_sprites(
                    stack,
                    self.tree_species.get(&stack.appearance).copied().unwrap_or_default(),
                    self.tree_variants.get(&stack.appearance).copied().unwrap_or_default(),
                    self.animation,
                )
            } else {
                sculpture_sprites_with_style(stack, self.animation, self.legacy_appearances.contains(&stack.appearance))
            };
            if let Some(age) = self.tree_births.get(&stack.appearance) {
                // A quick overshoot and settle, anchored at the snow rather than moving the physical object.
                let t = (*age / 0.55).clamp(0.0, 1.0);
                let scale = birth_scale(*age);
                let base = pixel_tree_base(stack.base);
                for sprite in &mut sprites {
                    sprite.position = base + (sprite.position - base) * scale;
                    sprite.size *= scale.max(0.01);
                    sprite.alpha *= (t * 6.0).min(1.0);
                }
            }
            groups.push(SculptureDraw { sprites, covers, control: false });
        }
        if let Some(icon) = self.icon {
            groups.push(SculptureDraw { sprites: vec![control_sprite(icon, self.tree, self.active)], covers: Vec::new(), control: true });
        }
        groups
    }
}

pub(crate) fn birth_scale(age: f32) -> f32 {
    let t = (age / 0.55).clamp(0.0, 1.0);
    1.0 - (-7.0 * t).exp() * (t * 10.0).cos()
}

impl Drop for Sculpt {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Snowball with a plus starts sculpting, a cross ends it, and four arrows move a tree.
pub(crate) fn control_sprite(position: V2, tree: bool, active: bool) -> Sprite {
    Sprite {
        position,
        size: ICON_RADIUS * 2.0,
        alpha: 1.0,
        mood: [19.0, 0.0],
        pose: [
            if tree {
                2.0
            } else if active {
                1.0
            } else {
                0.0
            },
            0.0,
            0.0,
            0.0,
        ],
        ..Sprite::default()
    }
}

// Sculpture-only material codes are carried in the otherwise unused mood fields.
fn material(position: V2, size: f32, color: [f32; 3], kind: f32, rotation: f32, detail: f32) -> Sprite {
    Sprite { position, size, alpha: 1.0, tint: color, tint_amount: 1.0, mood: [kind, rotation], pose: [detail, 0.0, 0.0, 0.0], ..Sprite::default() }
}

pub fn cone_sprites(position: V2, hovered: bool, species: Species) -> Vec<Sprite> {
    let mut halo = material(position, 74.0, [1.0; 3], 8.0, 0.0, 0.0);
    halo.alpha = if hovered { 0.62 } else { 0.28 };
    let mut cone = material(position, crate::cone::BODY_SIZE, [0.48, 0.26, 0.11], 7.0, 0.0, 0.0);
    cone.pose[1] = species.code() as f32;
    vec![halo, cone]
}

fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |duration| duration.as_secs())
}

pub fn tree_radius(random: u64) -> f32 {
    25.0 + (random as u32 as f32 / u32::MAX as f32) * 11.0
}

pub const CELESTIAL_VARIANT: u8 = 4;
pub const RUBY_VARIANT: u8 = 5;
pub const ROSE_VARIANT: u8 = 6;

/// Five equal balls have height 8.8 radii; ordinary adult trees top out at 36 * 5.4.
pub fn celestial_radius(random: u64) -> f32 {
    let fraction = random as u32 as f32 / u32::MAX as f32;
    // Saved radii have four decimal places; round inward so reloads stay in range.
    let min = ((36.0_f32 * 5.4 / 8.8) * 10_000.0).ceil() / 10_000.0;
    let max = ((36.0_f32 * 5.4 * 1.5 / 8.8) * 10_000.0).floor() / 10_000.0;
    ((min + (max - min) * fraction) * 10_000.0).round() / 10_000.0
}

pub fn celestial_height(random: u64) -> f32 {
    celestial_radius(random) * 8.8
}

pub(crate) fn tree_stage_at(identity: u64, now: u64) -> u8 {
    let age = now.saturating_sub(identity >> 32);
    if age < 60 {
        0
    } else if age < 180 {
        1
    } else {
        2
    }
}

pub(crate) fn adult_tree(identity: u64) -> bool {
    tree_stage_at(identity, now_seconds()) == 2
}

fn radii_height(radii: &[f32]) -> f32 {
    let mut height = 0.0;
    let mut previous: Option<f32> = None;
    for &radius in radii {
        if let Some(previous) = previous {
            height -= previous.min(radius) * 0.3;
        }
        height += radius * 2.0;
        previous = Some(radius);
    }
    height
}

fn displayed_tree_height(full_height: f32, identity: u64, now: u64) -> f32 {
    let height = match tree_stage_at(identity, now) {
        0 => (full_height * 0.13).max(12.0),
        1 => full_height * 0.48,
        _ => full_height,
    };
    height.round().max(2.0)
}

/// One hundred simulation units represent one metre, consistently across monitors and DPI settings.
fn collection_height_mm(full_height: f32, identity: u64, now: u64) -> u32 {
    (displayed_tree_height(full_height, identity, now) * 10.0) as u32
}

fn tree_height(stack: &snowhush_core::snowmen::Stack) -> f32 {
    match tree_stage_at(stack.appearance, now_seconds()) {
        0 => (stack.height() * 0.13).max(12.0),
        1 => stack.height() * 0.48,
        _ => stack.height(),
    }
}

fn tree_handle(stack: &snowhush_core::snowmen::Stack, size: (f32, f32)) -> V2 {
    V2::new(
        stack.base.x.clamp(ICON_RADIUS, (size.0 - ICON_RADIUS).max(ICON_RADIUS)),
        (stack.base.y + tree_height(stack) + ICON_RADIUS + 4.0).min((size.1 - ICON_RADIUS).max(ICON_RADIUS)),
    )
}

fn pixel_tree_base(base: V2) -> V2 {
    // The swap chain is DIP-sized. Keep the art on its pixel grid without changing physical coordinates.
    V2::new(base.x.round(), base.y.round())
}

pub(crate) fn tree_sprites(stack: &snowhush_core::snowmen::Stack, species: Species, variant: u8, time: f64) -> Vec<Sprite> {
    let now = now_seconds();
    let stage = tree_stage_at(stack.appearance, now);
    let full_height = stack.height_from_radii();
    let height = displayed_tree_height(full_height, stack.appearance, now);
    let base = pixel_tree_base(stack.base);
    if stage > 0 {
        let mut tree = material(base + V2::new(0.0, height * 0.5), height, [1.0; 3], if stage == 2 { 6.0 } else { 9.0 }, 0.0, 0.0);
        tree.pose[1] = species.code() as f32;
        tree.pose[2] = variant.min(ROSE_VARIANT) as f32;
        tree.pose[3] = time as f32;
        let mut sprites = vec![tree];
        if stage == 2 {
            if variant == CELESTIAL_VARIANT {
                append_celestial_lights(&mut sprites, base, height, time);
            } else if !crate::star_event::LEGENDARY_VARIANTS.contains(&variant) {
                append_tree_lights(&mut sprites, stack.appearance, base, height, species, variant, time);
            }
        }
        return sprites;
    }
    // Sprouts are drawn on a tiny species-specific pixel grid so they stay legible at desktop size.
    vec![material(base + V2::new(0.0, height * 0.5), height, [1.0; 3], 12.0, 0.0, species.code() as f32)]
}

fn append_celestial_lights(sprites: &mut Vec<Sprite>, base: V2, height: f32, time: f64) {
    // Four bright, animated swags span the crown; each light has its own phase.
    for row in 0..4 {
        let y = 0.22 + row as f32 * 0.16;
        let half_width = 0.29 * (1.0 - y * 0.65);
        for i in 0..15 {
            let t = i as f32 / 14.0;
            let x = (t * 2.0 - 1.0) * half_width;
            let sag = 0.045 * (1.0 - (2.0 * t - 1.0).powi(2));
            let wave = 0.5 + 0.5 * (time * 2.2 + (i + row * 7) as f64 * 0.7).sin() as f32;
            let mut light =
                material(base + V2::new(x * height, (y - sag) * height), (height * 0.032).clamp(5.0, 9.0), [1.0, 0.78, 0.25], 10.0, 0.0, 0.0);
            light.alpha = 0.75 + 0.25 * wave;
            sprites.push(light);
        }
    }
}

fn append_tree_lights(sprites: &mut Vec<Sprite>, appearance: u64, base: V2, height: f32, species: Species, variant: u8, time: f64) {
    // Positions follow the tiered garland inside the adult crown, relative to its snow contact.
    const LIGHTS: [(f32, f32); 14] = [
        (-0.13, 0.20),
        (0.02, 0.22),
        (0.18, 0.24),
        (0.11, 0.33),
        (-0.08, 0.36),
        (-0.15, 0.45),
        (0.02, 0.47),
        (0.15, 0.49),
        (0.09, 0.58),
        (-0.07, 0.60),
        (-0.09, 0.70),
        (0.04, 0.72),
        (0.06, 0.81),
        (-0.02, 0.87),
    ];
    let seed = (appearance as u32 as f64 / u32::MAX as f64) * std::f64::consts::TAU;
    for (index, &(x, y)) in LIGHTS.iter().enumerate() {
        let phase = seed + index as f64 * 1.71;
        let wave = 0.5 + 0.5 * (time * 1.55 + phase).sin() as f32;
        let color = match (species, variant) {
            (Species::Spruce, 1) => [0.75, 0.93, 1.0],
            (Species::Pine, 0) => [0.68, 0.84, 1.0],
            (Species::Fir, 0) => [1.0, 0.84, 0.83],
            _ => [1.0, 0.79, 0.41],
        };
        let px = if variant % 2 == 0 { x } else { -x * 0.9 };
        let mut light = material(base + V2::new(px * height, y * height), (height * 0.045).clamp(3.5, 7.5), color, 10.0, 0.0, 0.0);
        light.alpha = 0.20 + 0.65 * wave * wave;
        sprites.push(light);
    }
}

#[cfg(test)]
pub(crate) fn sculpture_sprites(stack: &snowhush_core::snowmen::Stack, time: f64) -> Vec<Sprite> {
    sculpture_sprites_with_style(stack, time, false)
}

fn sculpture_sprites_with_style(stack: &snowhush_core::snowmen::Stack, time: f64, legacy: bool) -> Vec<Sprite> {
    let mut sprites = Vec::new();
    let seed = stack.appearance;
    let variant = |shift: u32| ((seed >> shift) & 255) as f32 / 255.0;
    // Pivot at the contact with the snow. Collision and pointer coordinates remain unchanged.
    let angle = if stack.snowman() {
        let amplitude = 0.035_f32.min(2.0 / stack.height());
        ((time * (1.0 + variant(8) * 0.35) as f64 + (variant(16) * 2.0 * PI) as f64).sin() as f32) * amplitude
    } else {
        0.0
    };
    let rotate = |point: V2| {
        let p = point - stack.base;
        stack.base + V2::new(p.x * angle.cos() - p.y * angle.sin(), p.x * angle.sin() + p.y * angle.cos())
    };
    let balls: Vec<_> = stack.ball_centres().collect();
    if stack.snowman() {
        let (body, r) = balls[1];
        let wood = [0.35 + variant(24) * 0.11, 0.23 + variant(32) * 0.08, 0.14 + variant(40) * 0.06];
        for side in [-1.0, 1.0] {
            let asymmetry = if side < 0.0 { variant(48) } else { variant(56) };
            let start = body + V2::new(side * r * 0.73, r * 0.08);
            let elbow = body + V2::new(side * r * 1.38, r * (0.35 + asymmetry * 0.12));
            let end = body + V2::new(side * r * (1.90 + asymmetry * 0.18), r * (0.68 + asymmetry * 0.16));
            twig(&mut sprites, rotate(start), rotate(elbow), (r * 0.15).clamp(1.2, 2.8), wood);
            twig(&mut sprites, rotate(elbow), rotate(end), (r * 0.11).clamp(1.0, 2.1), wood);
            let fork = elbow + (end - elbow) * 0.58;
            twig(&mut sprites, rotate(fork), rotate(fork + V2::new(side * r * 0.16, r * 0.38)), (r * 0.08).clamp(0.9, 1.6), wood);
        }
    }
    for (index, &(centre, radius)) in balls.iter().enumerate() {
        sprites.push(material(rotate(centre), radius * 2.0, [1.0; 3], 1.0, angle, variant(((index % 7) * 8) as u32)));
    }
    if stack.snowman() && legacy {
        let (head, radius) = balls[2];
        let stone = [0.20 + variant(32) * 0.09, 0.25 + variant(40) * 0.08, 0.31 + variant(48) * 0.08];
        let eye = (radius * 0.16).clamp(1.0, 2.4);
        for x in [-0.34, 0.34] {
            sprites.push(material(rotate(head + V2::new(radius * x, radius * 0.28)), eye * 2.0, stone, 2.0, angle, variant(24)));
        }
        let direction = if seed & 1 == 0 { 1.0 } else { -1.0 };
        let carrot = [0.91 + variant(8) * 0.08, 0.34 + variant(16) * 0.13, 0.055 + variant(24) * 0.05];
        let nose = head + V2::new(direction * radius * 0.42, -radius * 0.02);
        sprites.push(material(rotate(nose), radius * 1.65, carrot, 3.0, angle + if direction < 0.0 { PI } else { 0.0 }, 0.0));
        // A small curved smile and a few softly lit stone buttons, as in the selected concept.
        let smile = [0.27, 0.23, 0.22];
        let points = [(-0.24, -0.40), (-0.12, -0.47), (0.0, -0.49), (0.12, -0.47), (0.24, -0.40)];
        for pair in points.windows(2) {
            let a = head + V2::new(radius * pair[0].0, radius * pair[0].1);
            let b = head + V2::new(radius * pair[1].0, radius * pair[1].1);
            twig(&mut sprites, rotate(a), rotate(b), (radius * 0.065).clamp(0.75, 1.4), smile);
        }
        for (index, y) in [(1, 0.05), (0, 0.22), (0, -0.27)] {
            let (body, r) = balls[index];
            sprites.push(material(rotate(body + V2::new(0.0, r * y)), (r * 0.29).clamp(2.2, 5.5), stone, 2.0, angle, variant(16)));
        }
    } else if stack.snowman() {
        // Independent deterministic samples keep every feature attached to the stack's identity.
        let sample = |slot: u64| {
            let mut x = seed.wrapping_add(slot.wrapping_mul(0x9e3779b97f4a7c15));
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
            ((x ^ (x >> 31)) >> 40) as f32 / 16777215.0
        };
        let (head, radius) = balls[2];
        let stone = [0.20 + variant(32) * 0.09, 0.25 + variant(40) * 0.08, 0.31 + variant(48) * 0.08];
        let eye_size = (radius * (0.14 + sample(1) * 0.04)).clamp(1.0, 2.4);
        let eye_spacing = 0.30 + sample(2) * 0.08;
        let eye_height = 0.27 + sample(3) * 0.06;
        for side in [-1.0, 1.0] {
            sprites.push(material(
                rotate(head + V2::new(radius * side * eye_spacing, radius * eye_height)),
                eye_size * 2.0,
                stone,
                2.0,
                angle,
                sample(4),
            ));
        }
        let direction = if seed & 1 == 0 { 1.0 } else { -1.0 };
        let length = radius * (1.30 + sample(5) * 0.45);
        let tilt = (sample(6) - 0.5) * 0.30;
        let carrot = [0.91 + sample(7) * 0.08, 0.32 + sample(8) * 0.15, 0.045 + sample(9) * 0.07];
        let nose = head + V2::new(direction * length * 0.25 * tilt.cos(), length * 0.25 * tilt.sin() - radius * 0.02);
        let mut sprite =
            material(rotate(nose), length, carrot, 5.0, angle + if direction < 0.0 { PI - tilt } else { tilt }, 0.17 + sample(10) * 0.12);
        sprite.pose[1] = sample(11); // Controls the taper and gentle bend.
        sprites.push(sprite);

        let smile = [0.27, 0.23, 0.22];
        let width = 0.21 + sample(12) * 0.08;
        let depth = 0.055 + sample(13) * 0.065;
        let height = -0.41 - sample(14) * 0.025;
        let points: Vec<_> = (-2..=2)
            .map(|i| {
                let x = i as f32 / 2.0;
                head + V2::new(radius * width * x, radius * (height - depth * (1.0 - x * x)))
            })
            .collect();
        for pair in points.windows(2) {
            twig(&mut sprites, rotate(pair[0]), rotate(pair[1]), (radius * 0.065).clamp(0.75, 1.4), smile);
        }
        let count = 2 + (sample(15) * 3.0).floor().min(2.0) as usize;
        let button_color = [0.19 + sample(16) * 0.12, 0.23 + sample(17) * 0.11, 0.28 + sample(18) * 0.11];
        for i in 0..count {
            let (index, y) = match (count, i) {
                (2, 0) => (1, 0.02),
                (2, _) => (0, 0.05),
                (3, 0) => (1, 0.05),
                (3, 1) => (0, 0.22),
                (3, _) => (0, -0.27),
                (_, 0) => (1, 0.30),
                (_, 1) => (1, -0.28),
                (_, 2) => (0, 0.25),
                (_, _) => (0, -0.27),
            };
            let (body, r) = balls[index];
            let offset = (sample(20 + i as u64) - 0.5) * 0.16;
            let size = (r * (0.24 + sample(24 + i as u64) * 0.10)).clamp(2.2, 5.5);
            sprites.push(material(rotate(body + V2::new(r * offset, r * y)), size, button_color, 2.0, angle, sample(28 + i as u64)));
        }
    }
    sprites
}

fn twig(sprites: &mut Vec<Sprite>, start: V2, end: V2, width: f32, color: [f32; 3]) {
    let delta = end - start;
    let size = (delta.length() + width).max(1.0);
    sprites.push(material(start + delta * 0.5, size, color, 4.0, delta.y.atan2(delta.x), width / size));
}

// The regular cursor sweeps a shallow hole in the snow. Look at the rim as well as directly under it,
// so the sculpting button can still be reached after moving the pointer into a drift.
fn nearby_snow(simulation: &SnowSimulation, point: V2, reach: f32) -> Option<(isize, V2)> {
    [0.0, -6.0, 6.0, -12.0, 12.0, -18.0, 18.0]
        .into_iter()
        .filter_map(|dx| simulation.snow_surface_near(V2::new(point.x + dx, point.y), reach))
        .min_by(|(_, a), (_, b)| (*a - point).length_squared().total_cmp(&(*b - point).length_squared()))
}

pub(crate) fn window_key(id: isize) -> Option<String> {
    let window = HWND(id as _);
    if !unsafe { IsWindow(Some(window)).as_bool() } {
        return None;
    }
    let mut process = 0;
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut process));
    }
    let mut title = [0u16; 1024];
    let length = unsafe { GetWindowTextW(window, &mut title) }.max(0) as usize;
    Some(format!(
        "{}\n{}\n{}",
        tracker::process_name(process).to_lowercase(),
        tracker::class_name(window),
        String::from_utf16_lossy(&title[..length])
    ))
}

fn window_kind(key: &str) -> String {
    key.split('\n').take(2).collect::<Vec<_>>().join("\n")
}

fn matching_window<'a>(key: &str, unique_saved_kind: bool, identities: &'a [(&WindowSurface, String)]) -> Option<(&'a WindowSurface, &'a String)> {
    let mut exact = identities.iter().filter(|(_, identity)| identity == key);
    if let Some(first) = exact.next() {
        return exact.next().is_none().then_some((first.0, &first.1));
    }
    if !unique_saved_kind {
        return None;
    }
    let kind = window_kind(key);
    let mut candidates = identities.iter().filter(|(_, identity)| window_kind(identity) == kind);
    let first = candidates.next()?;
    candidates.next().is_none().then_some((first.0, &first.1))
}

fn encode(saved: &[Saved]) -> String {
    let storage = saved.iter().any(|entry| matches!(entry.anchor, Anchor::Stored(_)));
    let variants = storage || saved.iter().any(|entry| entry.variant != 0);
    let extended = storage || saved.iter().any(|entry| entry.species != Species::Spruce);
    let celestial = saved.iter().any(|entry| entry.variant == CELESTIAL_VARIANT);
    let ruby = saved.iter().any(|entry| entry.variant == RUBY_VARIANT);
    let rose = saved.iter().any(|entry| entry.variant == ROSE_VARIANT);
    let mut text = String::from(if rose {
        "snowhush-sculptures-9\n"
    } else if ruby {
        "snowhush-sculptures-8\n"
    } else if storage {
        "snowhush-sculptures-7\n"
    } else if celestial {
        "snowhush-sculptures-6\n"
    } else if variants {
        "snowhush-sculptures-5\n"
    } else if extended {
        "snowhush-sculptures-4\n"
    } else {
        "snowhush-sculptures-3\n"
    });
    for entry in saved {
        let anchor = match &entry.anchor {
            Anchor::Ground(x) => format!("g;{x:.6}"),
            Anchor::Window(key, x) => format!("w;{};{x:.6}", hex(key.as_bytes())),
            Anchor::Falling(x, y) => format!("f;{x:.6};{y:.6}"),
            Anchor::Stored(x) => format!("s;{x:.6}"),
        };
        let radii = entry.radii.iter().map(|r| format!("{r:.4}")).collect::<Vec<_>>().join(",");
        text.push_str(&format!("{anchor};{radii};{:016x};{}", entry.appearance, if entry.legacy { 0 } else { 1 }));
        if extended || variants {
            text.push_str(&format!(";{}", entry.species.code()));
        }
        if variants {
            text.push_str(&format!(";{}", entry.variant));
        }
        text.push('\n');
    }
    text
}

fn decode(text: &str) -> Option<Vec<Saved>> {
    let mut lines = text.lines();
    let version = match lines.next()? {
        "snowhush-sculptures-1" => 1,
        "snowhush-sculptures-2" => 2,
        "snowhush-sculptures-3" => 3,
        "snowhush-sculptures-4" => 4,
        "snowhush-sculptures-5" => 5,
        "snowhush-sculptures-6" => 6,
        "snowhush-sculptures-7" => 7,
        "snowhush-sculptures-8" => 8,
        "snowhush-sculptures-9" => 9,
        _ => return None,
    };
    let mut entries = Vec::new();
    for line in lines {
        if entries.len() >= 64 {
            return None;
        }
        let mut parts = line.split(';');
        let number = |value: Option<&str>| -> Option<f32> {
            let number: f32 = value?.parse().ok()?;
            (number.is_finite() && (0.0..=1.0).contains(&number)).then_some(number)
        };
        let anchor = match parts.next()? {
            "g" => Anchor::Ground(number(parts.next())?),
            "w" => Anchor::Window(String::from_utf8(unhex(parts.next()?)?).ok()?, number(parts.next())?),
            "f" => Anchor::Falling(number(parts.next())?, number(parts.next())?),
            "s" if version >= 7 => Anchor::Stored(number(parts.next())?),
            _ => return None,
        };
        let radii: Vec<f32> = parts.next()?.split(',').map(str::parse).collect::<Result<_, _>>().ok()?;
        let appearance = if version >= 2 {
            let value = parts.next()?;
            if value.len() != 16 {
                return None;
            }
            u64::from_str_radix(value, 16).ok()?
        } else {
            // Stable even if a legacy window cannot yet be matched or saving is unavailable.
            line.bytes()
                .chain((entries.len() as u64).to_le_bytes())
                .fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3))
        };
        let legacy = if version >= 3 {
            match parts.next()? {
                "0" => true,
                "1" => false,
                _ => return None,
            }
        } else {
            true
        };
        let species = if version >= 4 { Species::from_code(parts.next()?.parse().ok()?)? } else { Species::Spruce };
        let variant = if version >= 5 {
            match parts.next()? {
                "0" => 0,
                "1" => 1,
                "2" => 2,
                "3" => 3,
                "4" if version >= 6 => 4,
                "5" if version >= 8 => 5,
                "6" if version >= 9 => 6,
                _ => return None,
            }
        } else {
            0
        };
        if parts.next().is_some()
            || radii.is_empty()
            || radii.len() > MAX_BALLS_PER_STACK
            || radii.iter().any(|r| !r.is_finite() || !(5.0..=36.0).contains(r))
        {
            return None;
        }
        entries.push(Saved { anchor, radii, appearance, legacy, species, variant });
    }
    Some(entries)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        value.push(DIGITS[(byte >> 4) as usize] as char);
        value.push(DIGITS[(byte & 15) as usize] as char);
    }
    value
}

pub(crate) fn unhex(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0 {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            };
            Some((digit(pair[0])? << 4) | digit(pair[1])?)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowhush_core::geometry::Rect;
    use snowhush_core::simulation::Mode;

    #[test]
    fn stored_tree_round_trips_with_its_identity_and_appearance() {
        let item = Saved {
            anchor: Anchor::Stored(0.42),
            radii: vec![30.0; 3],
            appearance: 0x1234567800000001,
            legacy: false,
            species: Species::Fir,
            variant: 2,
        };
        let encoded = encode(&[item.clone()]);
        assert!(encoded.starts_with("snowhush-sculptures-7\n"));
        assert_eq!(decode(&encoded), Some(vec![item]));
        assert!(decode(&encoded.replacen("snowhush-sculptures-7", "snowhush-sculptures-6", 1)).is_none());
    }

    fn blank() -> Sculpt {
        Sculpt {
            objects: Snowmen::default(),
            tree: false,
            active: false,
            icon: None,
            hover: None,
            hovered_tree: None,
            previous: None,
            display: String::new(),
            pending: Vec::new(),
            legacy_appearances: HashSet::new(),
            widths: HashMap::new(),
            keys: HashMap::new(),
            resolve_delay: 0.0,
            last_saved: String::new(),
            save_delay: 0.0,
            writable: false,
            size: (500.0, 400.0),
            animation: 0.0,
            tree_stages: HashMap::new(),
            tree_births: HashMap::new(),
            tree_species: HashMap::new(),
            tree_variants: HashMap::new(),
        }
    }

    #[test]
    fn tree_growth_thresholds_and_saved_identity_survive_restart() {
        let born = 1_700_000_000_u64;
        let id = (born << 32) | 123;
        assert_eq!(tree_stage_at(id, born + 59), 0);
        assert_eq!(tree_stage_at(id, born + 60), 1);
        assert_eq!(tree_stage_at(id, born + 179), 1);
        assert_eq!(tree_stage_at(id, born + 180), 2);
        let saved = Saved { anchor: Anchor::Ground(0.4), radii: vec![30.0; 3], appearance: id, legacy: false, species: Species::Fir, variant: 1 };
        let restored = decode(&encode(&[saved.clone()])).unwrap().remove(0);
        assert_eq!(restored, saved);
        assert_eq!(tree_stage_at(restored.appearance, born + 100), 1);
    }

    #[test]
    fn collection_height_tracks_growth_and_is_the_same_when_stored() {
        let born = 1_700_000_000_u64;
        let id = (born << 32) | 123;
        let radii = vec![30.0; 3];
        let full = radii_height(&radii);
        assert_eq!(collection_height_mm(full, id, born + 59), 210);
        assert_eq!(collection_height_mm(full, id, born + 60), 780);
        assert_eq!(collection_height_mm(full, id, born + 180), 1620);
        let mut garden = blank();
        assert!(garden.objects.restore_with_appearance(V2::new(100.0, 0.0), radii.clone(), Some((0, 100.0)), id));
        let placed = garden.collection_trees()[0].4;
        garden.objects.take_resting(id).unwrap();
        garden.pending.push(Saved { anchor: Anchor::Stored(0.2), radii, appearance: id, legacy: false, species: Species::Spruce, variant: 0 });
        assert_eq!(garden.collection_trees()[0].4, placed);
        assert_eq!(placed, 1620);
    }

    #[test]
    fn celestial_tree_size_save_and_lights_are_distinct_and_persistent() {
        let normal_max = tree_radius(u32::MAX as u64) * 5.4;
        assert!((celestial_height(0) - normal_max).abs() < 0.001);
        assert!((celestial_height(u32::MAX as u64) - normal_max * 1.5).abs() < 0.001);
        let mut garden = blank();
        garden.tree = true;
        garden.writable = true;
        let planted = garden.plant_legendary(V2::new(150.0, 0.0), (0, 150.0), 123, CELESTIAL_VARIANT);
        let duplicate = garden.plant_legendary(V2::new(350.0, 0.0), (0, 350.0), 124, CELESTIAL_VARIANT);
        garden.writable = false;
        assert!(planted);
        assert!(garden.has_legendary_tree(CELESTIAL_VARIANT));
        assert!(!duplicate);
        let stack = &garden.objects.stacks()[0];
        let saved = Saved {
            anchor: Anchor::Ground(0.3),
            radii: stack.radii.clone(),
            appearance: stack.appearance,
            legacy: false,
            species: Species::Spruce,
            variant: CELESTIAL_VARIANT,
        };
        let encoded = encode(&[saved.clone()]);
        assert!(encoded.starts_with("snowhush-sculptures-6\n"));
        let restored = decode(&encoded).unwrap().remove(0);
        assert_eq!((restored.appearance, restored.variant, restored.species), (saved.appearance, saved.variant, saved.species));
        assert!((restored.radii[0] - saved.radii[0]).abs() < 0.0001);
        // Use an old identity to render the adult stage regardless of wall clock.
        let mut adult = Snowmen::default();
        assert!(adult.restore_with_appearance(V2::new(150.0, 0.0), stack.radii.clone(), Some((0, 150.0)), 1));
        let first = tree_sprites(&adult.stacks()[0], Species::Spruce, CELESTIAL_VARIANT, 0.0);
        let later = tree_sprites(&adult.stacks()[0], Species::Spruce, CELESTIAL_VARIANT, 1.0);
        assert_eq!(first[0].pose[2], CELESTIAL_VARIANT as f32);
        assert_eq!(first.iter().filter(|sprite| sprite.mood[0] == 10.0).count(), 60);
        assert!(first.iter().zip(&later).skip(1).any(|(a, b)| (a.alpha - b.alpha).abs() > 0.1));
    }

    #[test]
    fn legendary_designs_can_coexist_and_stored_ruby_survives_reload() {
        let mut garden = blank();
        garden.tree = true;
        garden.writable = true;
        assert!(garden.plant_legendary(V2::new(150.0, 0.0), (0, 150.0), 123, CELESTIAL_VARIANT));
        assert!(garden.plant_legendary(V2::new(350.0, 0.0), (0, 350.0), 124, RUBY_VARIANT));
        assert!(!garden.plant_legendary(V2::new(450.0, 0.0), (0, 450.0), 125, RUBY_VARIANT));
        let ruby = garden.objects.stacks().iter().find(|tree| garden.tree_style(tree.appearance).1 == RUBY_VARIANT).unwrap().clone();
        let saved = Saved {
            anchor: Anchor::Stored(0.4),
            radii: ruby.radii.clone(),
            appearance: ruby.appearance,
            legacy: false,
            species: Species::Spruce,
            variant: RUBY_VARIANT,
        };
        let encoded = encode(&[saved.clone()]);
        assert!(encoded.starts_with("snowhush-sculptures-8\n"));
        let restored = decode(&encoded).unwrap().remove(0);
        assert_eq!(restored.variant, RUBY_VARIANT);
        assert_eq!(restored.anchor, saved.anchor);
        garden.objects.take_resting(ruby.appearance).unwrap();
        garden.pending.push(restored);
        assert!(garden.has_legendary_tree(RUBY_VARIANT));
        garden.pending.clear();
        assert!(!garden.has_legendary_tree(RUBY_VARIANT));
        assert!(garden.has_legendary_tree(CELESTIAL_VARIANT));
        assert!(garden.plant_legendary(V2::new(350.0, 0.0), (0, 350.0), 126, RUBY_VARIANT));
    }

    #[test]
    fn rose_tree_is_unique_and_survives_storage_and_reload() {
        let mut garden = blank();
        garden.tree = true;
        garden.writable = true;
        assert!(garden.plant_legendary(V2::new(150.0, 0.0), (0, 150.0), 123, CELESTIAL_VARIANT));
        assert!(garden.plant_legendary(V2::new(350.0, 0.0), (0, 350.0), 124, RUBY_VARIANT));
        assert!(garden.plant_legendary(V2::new(550.0, 0.0), (0, 550.0), 125, ROSE_VARIANT));
        assert!(!garden.plant_legendary(V2::new(750.0, 0.0), (0, 750.0), 126, ROSE_VARIANT));
        let rose = garden.objects.stacks().iter().find(|tree| garden.tree_style(tree.appearance).1 == ROSE_VARIANT).unwrap().clone();
        let saved = Saved {
            anchor: Anchor::Stored(0.4),
            radii: rose.radii.clone(),
            appearance: rose.appearance,
            legacy: false,
            species: Species::Spruce,
            variant: ROSE_VARIANT,
        };
        let encoded = encode(&[saved.clone()]);
        assert!(encoded.starts_with("snowhush-sculptures-9\n"));
        assert!(decode(&encoded.replacen("sculptures-9", "sculptures-8", 1)).is_none());
        let restored = decode(&encoded).unwrap().remove(0);
        assert_eq!((restored.variant, &restored.anchor), (ROSE_VARIANT, &saved.anchor));
        garden.objects.take_resting(rose.appearance).unwrap();
        garden.pending.push(restored);
        assert!(garden.has_legendary_tree(ROSE_VARIANT));
        let mut adult = Snowmen::default();
        assert!(adult.restore_with_appearance(V2::new(150.0, 0.0), rose.radii, Some((0, 150.0)), 1));
        let sprites = tree_sprites(&adult.stacks()[0], Species::Spruce, ROSE_VARIANT, 0.0);
        assert_eq!(sprites[0].pose[2], ROSE_VARIANT as f32);
        assert_eq!(sprites.len(), 1);
    }

    #[test]
    fn tree_population_counts_young_and_adult_trees_but_not_planted_cones() {
        let mut sculpt = blank();
        let now = now_seconds();
        for (index, age, species) in [(1, 10, Species::Spruce), (2, 90, Species::Pine), (3, 240, Species::Pine), (4, 240, Species::Fir)] {
            let id = (now.saturating_sub(age) << 32) | index;
            assert!(sculpt.objects.restore_with_appearance(V2::new(index as f32 * 50.0, 0.0), vec![20.0], Some((0, 0.0)), id));
            sculpt.tree_species.insert(id, species);
        }
        assert_eq!(sculpt.tree_population(), [0, 2, 1]);
    }

    #[test]
    fn adult_garlands_animate_for_all_variants_and_every_species() {
        let mut objects = Snowmen::default();
        assert!(objects.restore_with_appearance(V2::new(100.0, 0.0), vec![30.0; 3], Some((0, 100.0)), 7));
        for species in [Species::Spruce, Species::Pine, Species::Fir] {
            for variant in 0..=3 {
                let first = tree_sprites(&objects.stacks()[0], species, variant, 0.0);
                let later = tree_sprites(&objects.stacks()[0], species, variant, 1.0);
                assert_eq!(first[0].pose[1], species.code() as f32);
                assert_eq!(first[0].pose[2], variant as f32);
                assert_eq!(first.iter().filter(|sprite| sprite.mood[0] == 10.0).count(), 14);
                assert!(first.iter().zip(&later).skip(1).any(|(a, b)| (a.alpha - b.alpha).abs() > 0.1));
            }
        }
    }

    #[test]
    fn tree_art_and_lights_follow_the_same_pixel_grid_without_changing_physics() {
        let mut objects = Snowmen::default();
        let base = V2::new(100.05, 20.05);
        assert!(objects.restore_with_appearance(base, vec![28.37; 3], Some((0, base.x)), 7));
        let before = tree_sprites(&objects.stacks()[0], Species::Fir, 1, 0.5);
        let fractional = V2::new(100.45, 20.45);
        assert!(objects.relocate_resting(7, fractional, (0, fractional.x)));
        assert_eq!(objects.stacks()[0].base, fractional);
        let after = tree_sprites(&objects.stacks()[0], Species::Fir, 1, 0.5);
        assert!(before.iter().zip(&after).all(|(a, b)| a.position == b.position && a.size == b.size));
        let one_pixel = V2::new(101.05, 21.05);
        assert!(objects.relocate_resting(7, one_pixel, (0, one_pixel.x)));
        let shifted = tree_sprites(&objects.stacks()[0], Species::Fir, 1, 0.5);
        assert!(
            before.iter().zip(&shifted).all(|(a, b)| ((b.position - a.position) - V2::new(1.0, 1.0)).length_squared() < 0.000001 && a.size == b.size)
        );
    }

    #[test]
    fn planting_creates_a_rooted_sprout_with_a_short_settling_animation() {
        let mut garden = blank();
        garden.tree = true;
        assert!(garden.plant(V2::new(140.0, 25.0), (42, 30.0), 7, Species::Pine));
        let tree = &garden.objects.stacks()[0];
        assert_eq!(tree.base, V2::new(140.0, 25.0));
        assert_eq!(tree.support, Some((42, 30.0)));
        assert_eq!(tree_stage_at(tree.appearance, now_seconds()), 0);
        assert_eq!(garden.tree_births.get(&tree.appearance), Some(&0.0));
        assert_eq!(garden.tree_species.get(&tree.appearance), Some(&Species::Pine));
        assert!(birth_scale(0.0).abs() < 0.001);
        assert!(birth_scale(0.18) > 1.0);
        assert!((birth_scale(0.55) - 1.0).abs() < 0.01);
    }

    #[test]
    fn trees_use_snowman_fall_physics_and_keep_random_adult_size() {
        let mut garden = blank();
        garden.tree = true;
        let mut snow = SnowSimulation::new(500.0, 400.0, 0.0);
        assert!(garden.plant(V2::new(100.0, 0.0), (0, 100.0), 0, Species::Spruce));
        assert!(garden.plant(V2::new(200.0, 0.0), (0, 200.0), u32::MAX as u64, Species::Pine));
        assert!(garden.objects.stacks()[0].height() < garden.objects.stacks()[1].height());
        assert!(garden.objects.stacks()[1].height() <= 194.4 + 0.01);
        let at = V2::new(100.0, 5.0);
        garden.frame(&mut snow, &[], at, 0.016, &[], true);
        assert!(!garden.captures(at), "the tree body must pass clicks through");
        let handle = garden.icon.expect("hover reveals the handle");
        assert!(garden.captures(handle));
        garden.frame(&mut snow, &[], handle, 0.016, &[PointerEvent::Down(handle)], true);
        assert!(garden.objects.holding());
        let high = V2::new(100.0, 350.0);
        garden.frame(&mut snow, &[], high, 0.016, &[], true);
        garden.frame(&mut snow, &[], high, 0.016, &[PointerEvent::Up(high)], true);
        for _ in 0..120 {
            garden.frame(&mut snow, &[], high, 0.016, &[], true);
        }
        assert_eq!(garden.objects.stacks().len(), 1, "a hard fall breaks only the carried tree");
        assert_eq!(garden.objects.stacks()[0].base.x, 200.0);
    }

    #[test]
    fn saved_sculptures_round_trip_without_losing_window_identity() {
        let saved = vec![
            Saved {
                anchor: Anchor::Ground(0.25),
                radii: vec![20.0, 12.0, 6.0, 5.0],
                appearance: 42,
                legacy: false,
                species: Species::Spruce,
                variant: 0,
            },
            Saved {
                anchor: Anchor::Window("app.exe\nClass\nОкно; first".to_string(), 0.75),
                radii: vec![10.0],
                appearance: 43,
                legacy: true,
                species: Species::Pine,
                variant: 1,
            },
            Saved { anchor: Anchor::Falling(0.4, 0.6), radii: vec![8.0, 6.0], appearance: 44, legacy: false, species: Species::Fir, variant: 0 },
        ];
        assert_eq!(decode(&encode(&saved)), Some(saved));
        for variant in 2..=3 {
            let new_tree = Saved { anchor: Anchor::Ground(0.5), radii: vec![20.0], appearance: 45, legacy: false, species: Species::Fir, variant };
            assert_eq!(decode(&encode(&[new_tree.clone()])), Some(vec![new_tree]));
        }
        assert!(decode("new-version\ng;0.2;10\n").is_none());
        assert!(decode("snowhush-sculptures-1\ng;NaN;10\n").is_none());
        assert!(decode("snowhush-sculptures-1\nw;zz;0.2;10\n").is_none());
        assert!(decode("snowhush-sculptures-2\ng;0.2;10;bad\n").is_none());
        assert!(decode("snowhush-sculptures-3\ng;0.2;10;0000000000000001;2\n").is_none());
        assert!(decode("snowhush-sculptures-4\ng;0.2;10;0000000000000001;1;9\n").is_none());
        assert!(decode("snowhush-sculptures-5\ng;0.2;10;0000000000000001;1;2;4\n").is_none());
    }

    #[test]
    fn legacy_appearance_is_stable_and_survives_migration() {
        let legacy = "snowhush-sculptures-1\ng;0.25;20,12,6\nw;617070;0.5;15,10,6\n";
        let first = decode(legacy).unwrap();
        assert_eq!(first, decode(legacy).unwrap());
        assert_ne!(first[0].appearance, first[1].appearance);
        assert_eq!(decode(&encode(&first)), Some(first));
        let v2 = decode("snowhush-sculptures-2\ng;0.25;20,12,6;123456789abcdef0\n").unwrap();
        assert!(v2[0].legacy);
        assert_eq!(decode(&encode(&v2)), Some(v2));
        let v3 = decode("snowhush-sculptures-3\ng;0.25;20,12,6;123456789abcdef0;1\n").unwrap();
        assert_eq!(v3[0].species, Species::Spruce);
        assert_eq!(v3[0].variant, 0);
        assert!(encode(&v3).starts_with("snowhush-sculptures-3\n"));
        let v4 = decode("snowhush-sculptures-4\ng;0.25;20,12,6;123456789abcdef0;1;2\n").unwrap();
        assert_eq!((v4[0].species, v4[0].variant), (Species::Fir, 0));
    }

    #[test]
    fn appearance_and_sway_survive_carrying_and_adding_a_ball() {
        let mut sculpt = blank();
        let mut snow = SnowSimulation::new(500.0, 400.0, 0.0);
        assert!(sculpt.objects.restore_with_appearance(V2::new(100.0, 0.0), vec![15.0, 10.0, 6.0], Some((0, 100.0)), 0x123456789abcdef0));
        let original = sculpture_sprites(&sculpt.objects.stacks()[0], 0.0);
        let later = sculpture_sprites(&sculpt.objects.stacks()[0], 1.0);
        assert_eq!(original.len(), later.len());
        assert!(original.iter().zip(&later).any(|(a, b)| (a.position - b.position).length() > 0.1));
        assert!(original.iter().zip(&later).all(|(a, b)| (a.position - b.position).length() < 4.0));
        assert!(sculpt.objects.pick_up(V2::new(100.0, 15.0)));
        sculpt.objects.drag(V2::new(200.0, 15.0));
        sculpt.objects.release(V2::ZERO);
        for _ in 0..180 {
            sculpt.frame(&mut snow, &[], V2::ZERO, 1.0 / 60.0, &[], true);
        }
        let stack = &sculpt.objects.stacks()[0];
        assert_eq!(stack.appearance, 0x123456789abcdef0);
        let moved = sculpture_sprites(stack, 0.0);
        for (a, b) in original.iter().zip(&moved) {
            assert!(((b.position - a.position) - V2::new(100.0, 0.0)).length() < 0.01);
            assert_eq!(a.tint, b.tint);
            assert!(a.mood.iter().zip(b.mood).all(|(x, y)| (*x - y).abs() < 0.0001));
            assert!(a.pose.iter().zip(b.pose).all(|(x, y)| (*x - y).abs() < 0.0001));
        }
        let top = stack.base.y + stack.height();
        assert!(sculpt.objects.create(V2::new(200.0, top + 6.0), 6.0));
        sculpt.objects.release(V2::ZERO);
        assert_eq!(sculpt.objects.stacks()[0].appearance, 0x123456789abcdef0);
        assert_eq!(sculpt.objects.stacks()[0].radii.len(), 4);
        let saved = Saved {
            anchor: Anchor::Ground(0.4),
            radii: sculpt.objects.stacks()[0].radii.clone(),
            appearance: sculpt.objects.stacks()[0].appearance,
            legacy: false,
            species: Species::Spruce,
            variant: 0,
        };
        let restored = decode(&encode(&[saved.clone()])).unwrap().remove(0);
        let mut other = Snowmen::default();
        assert!(other.restore_with_appearance(V2::new(200.0, 0.0), restored.radii, Some((0, 200.0)), restored.appearance));
        let a = sculpture_sprites(&sculpt.objects.stacks()[0], 0.0);
        let b = sculpture_sprites(&other.stacks()[0], 0.0);
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(&b) {
            assert_eq!(a.position, b.position);
            assert_eq!(a.tint, b.tint);
            assert_eq!(a.mood, b.mood);
            assert_eq!(a.pose, b.pose);
        }
    }

    #[test]
    fn snowman_uses_distinct_materials_and_carrot_faces_both_ways() {
        let mut snowmen = Snowmen::default();
        for appearance in [0x123456789abcdef0, 0x123456789abcdef1] {
            assert!(snowmen.restore_with_appearance(V2::new(100.0, 0.0), vec![15.0, 10.0, 6.0], Some((0, 100.0)), appearance));
        }
        let first = sculpture_sprites_with_style(&snowmen.stacks()[0], 0.0, true);
        let second = sculpture_sprites_with_style(&snowmen.stacks()[1], 0.0, true);
        for sprites in [&first, &second] {
            assert_eq!(sprites.iter().filter(|sprite| sprite.mood[0] == 1.0).count(), 3);
            assert_eq!(sprites.iter().filter(|sprite| sprite.mood[0] == 2.0).count(), 5);
            assert_eq!(sprites.iter().filter(|sprite| sprite.mood[0] == 3.0).count(), 1);
            assert_eq!(sprites.iter().filter(|sprite| sprite.mood[0] == 4.0).count(), 10);
        }
        let first_nose = first.iter().find(|sprite| sprite.mood[0] == 3.0).unwrap();
        let second_nose = second.iter().find(|sprite| sprite.mood[0] == 3.0).unwrap();
        assert!(first_nose.mood[1].cos() > 0.9);
        assert!(second_nose.mood[1].cos() < -0.9);
        assert!(first_nose.position.x > 100.0);
        assert!(second_nose.position.x < 100.0);
    }

    #[test]
    fn new_appearances_have_varied_features_and_round_trip() {
        let mut counts = HashSet::new();
        let mut noses = HashSet::new();
        let mut eyes = HashSet::new();
        let mut smiles = HashSet::new();
        for seed in 0..64 {
            let saved = Saved {
                anchor: Anchor::Ground(0.2),
                radii: vec![15.0, 10.0, 6.0],
                appearance: seed,
                legacy: false,
                species: Species::Spruce,
                variant: 0,
            };
            let restored = decode(&encode(&[saved.clone()])).unwrap().remove(0);
            assert_eq!(restored, saved);
            let mut snowmen = Snowmen::default();
            assert!(snowmen.restore_with_appearance(V2::new(100.0, 0.0), restored.radii, Some((0, 100.0)), restored.appearance));
            let first = sculpture_sprites_with_style(&snowmen.stacks()[0], 0.75, restored.legacy);
            let again = sculpture_sprites_with_style(&snowmen.stacks()[0], 0.75, restored.legacy);
            for (a, b) in first.iter().zip(&again) {
                assert_eq!((a.position, a.size, a.tint, a.mood, a.pose), (b.position, b.size, b.tint, b.mood, b.pose));
            }
            let stones: Vec<_> = first.iter().filter(|sprite| sprite.mood[0] == 2.0).collect();
            counts.insert(stones.len() - 2);
            eyes.insert((stones[0].size.to_bits(), stones[0].position.x.to_bits(), stones[0].position.y.to_bits()));
            let nose = first.iter().find(|sprite| sprite.mood[0] == 5.0).unwrap();
            noses.insert((nose.size.to_bits(), nose.pose[0].to_bits(), nose.pose[1].to_bits(), nose.mood[1].to_bits(), nose.tint[1].to_bits()));
            let lines: Vec<_> = first.iter().filter(|sprite| sprite.mood[0] == 4.0).collect();
            smiles.insert((lines[6].position.x.to_bits(), lines[6].position.y.to_bits()));
        }
        assert_eq!(counts, HashSet::from([2, 3, 4]));
        assert!(noses.len() > 50 && eyes.len() > 50 && smiles.len() > 50);
    }

    #[test]
    fn old_save_keeps_legacy_rendering_after_migration() {
        let saved = decode("snowhush-sculptures-2\ng;0.25;15,10,6;123456789abcdef0\n").unwrap().remove(0);
        let migrated = decode(&encode(&[saved.clone()])).unwrap().remove(0);
        let mut objects = Snowmen::default();
        assert!(objects.restore_with_appearance(V2::new(100.0, 0.0), saved.radii, Some((0, 100.0)), saved.appearance));
        let before = sculpture_sprites_with_style(&objects.stacks()[0], 1.0, saved.legacy);
        let after = sculpture_sprites_with_style(&objects.stacks()[0], 1.0, migrated.legacy);
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(&after) {
            assert_eq!((a.position, a.size, a.tint, a.mood, a.pose), (b.position, b.size, b.tint, b.mood, b.pose));
        }
        assert_eq!(before.iter().filter(|sprite| sprite.mood[0] == 3.0).count(), 1);
        assert!(!before.iter().any(|sprite| sprite.mood[0] == 5.0));
    }

    #[test]
    fn icon_enters_mode_and_a_snowman_can_be_carried_and_released() {
        let mut sculpture = blank();
        let mut snow = SnowSimulation::new(500.0, 400.0, 0.0);
        assert!(sculpture.objects.restore(V2::new(100.0, 0.0), vec![12.0, 8.0, 5.0], Some((0, 100.0))));
        let over = V2::new(100.0, 12.0);
        for _ in 0..24 {
            sculpture.frame(&mut snow, &[], over, 0.02, &[], true);
        }
        let icon = sculpture.icon.expect("hover shows the on-screen button");
        sculpture.frame(&mut snow, &[], icon, 0.02, &[PointerEvent::Down(icon), PointerEvent::Up(icon)], true);
        assert!(sculpture.active());
        sculpture.frame(&mut snow, &[], over, 0.02, &[PointerEvent::Down(over)], true);
        assert!(sculpture.objects.holding());
        let target = V2::new(120.0, 20.0);
        sculpture.frame(&mut snow, &[], target, 0.05, &[], true);
        sculpture.frame(&mut snow, &[], target, 0.05, &[PointerEvent::Up(target)], true);
        assert!(!sculpture.objects.holding());
        assert!(sculpture.objects.stacks()[0].snowman());
        sculpture.frame(&mut snow, &[], target, 0.05, &[PointerEvent::Exit], true);
        assert!(!sculpture.active());
    }

    #[test]
    fn balls_and_snowmen_can_be_picked_up_directly_in_every_weather_mode() {
        for modes in [&[Mode::Snow][..], &[Mode::Autumn], &[Mode::Rain], &[Mode::Blossom], &[Mode::Snow, Mode::Autumn]] {
            for radii in [vec![10.0], vec![15.0, 10.0, 6.0]] {
                let mut sculpture = blank();
                let mut simulation = SnowSimulation::new(500.0, 400.0, 0.0);
                simulation.set_modes(modes);
                assert!(sculpture.objects.restore(V2::new(100.0, 0.0), radii.clone(), Some((0, 100.0))));
                let at = V2::new(100.0, radii[0]);
                assert!(sculpture.captures(at), "{modes:?}: ball must receive clicks outside sculpt mode");
                assert!(!sculpture.sprites(&[], &simulation).is_empty(), "{modes:?}: sculpture stays visible");
                sculpture.frame(&mut simulation, &[], at, 0.016, &[PointerEvent::Down(at)], true);
                assert!(sculpture.objects.holding(), "{modes:?}: click picks up {radii:?}");
                assert_eq!(sculpture.objects.held_stack().unwrap().radii, radii);
                let moved = at + V2::new(50.0, 50.0);
                sculpture.frame(&mut simulation, &[], moved, 0.016, &[], true);
                sculpture.frame(&mut simulation, &[], moved, 0.016, &[PointerEvent::Up(moved)], true);
                assert!(!sculpture.objects.holding());
                assert_eq!(sculpture.objects.stacks().len(), 1);
                assert!((sculpture.objects.stacks()[0].base.x - 150.0).abs() < 0.01);
            }
        }
    }

    #[test]
    fn changing_weather_during_a_drag_keeps_the_snowman_in_hand() {
        let mut sculpture = blank();
        let mut simulation = SnowSimulation::new(500.0, 400.0, 0.0);
        assert!(sculpture.objects.restore(V2::new(100.0, 0.0), vec![15.0, 10.0, 6.0], Some((0, 100.0))));
        let at = V2::new(100.0, 15.0);
        sculpture.frame(&mut simulation, &[], at, 0.016, &[PointerEvent::Down(at)], true);
        simulation.set_mode(Mode::Rain);
        let moved = V2::new(130.0, 45.0);
        sculpture.frame(&mut simulation, &[], moved, 0.016, &[], true);
        assert!(sculpture.objects.holding());
        assert!(sculpture.objects.held_stack().unwrap().snowman());
        assert_eq!(sculpture.objects.held_stack().unwrap().base.x, 130.0);
    }

    #[test]
    fn clicking_a_snowman_wins_when_the_control_icon_overlaps_it() {
        let mut sculpture = blank();
        let mut simulation = SnowSimulation::new(500.0, 400.0, 0.0);
        assert!(sculpture.objects.restore(V2::new(100.0, 0.0), vec![15.0, 10.0, 6.0], Some((0, 100.0))));
        let at = V2::new(100.0, 15.0);
        sculpture.icon = Some(at);
        sculpture.frame(&mut simulation, &[], at, 0.016, &[PointerEvent::Down(at)], true);
        assert!(sculpture.objects.holding());
        assert!(sculpture.objects.held_stack().unwrap().snowman());
    }

    #[test]
    fn rolling_a_new_ball_uses_and_collects_the_existing_drift() {
        let mut sculpture = blank();
        let mut snow = SnowSimulation::new(500.0, 400.0, 0.8);
        let away = V2::new(-10_000.0, -10_000.0);
        for _ in 0..(90 * 60) {
            snow.step(1.0 / 60.0, away);
        }
        snow.set_intensity(0.0);
        let at = V2::new(100.0, snow.ground_surface(100.0));
        let before = snow.snow_available(0, at.x, 22.0);
        assert!(before >= PI * START_RADIUS * START_RADIUS);
        sculpture.active = true;
        sculpture.frame(&mut snow, &[], at, 0.02, &[PointerEvent::Down(at)], true);
        assert!(sculpture.objects.holding());
        assert!(snow.snow_available(0, at.x, 22.0) < before);
        for n in 1..=30 {
            let x = 100.0 + n as f32 * 2.0;
            let cursor = V2::new(x, snow.ground_surface(x));
            sculpture.frame(&mut snow, &[], cursor, 1.0 / 60.0, &[], true);
        }
        assert!(sculpture.objects.held_stack().unwrap().radii[0] > START_RADIUS);
        let release = V2::new(160.0, snow.ground_surface(160.0));
        sculpture.frame(&mut snow, &[], release, 1.0 / 60.0, &[PointerEvent::Up(release)], true);
        for _ in 0..180 {
            sculpture.frame(&mut snow, &[], release, 1.0 / 60.0, &[], true);
        }
        let ball = &sculpture.objects.stacks()[0];
        assert!((ball.base.y - snow.ground_surface(ball.base.x)).abs() < 1.0, "released ball must rest on the snow");
        let radius = ball.radii[0];
        let centre = V2::new(ball.base.x, ball.base.y + radius);
        sculpture.frame(&mut snow, &[], centre, 1.0 / 60.0, &[PointerEvent::Down(centre)], true);
        assert!(sculpture.objects.holding(), "released ball can be picked up again");
        for n in 1..=20 {
            let x = centre.x + n as f32 * 2.0;
            let cursor = V2::new(x, snow.ground_surface(x) + radius);
            sculpture.frame(&mut snow, &[], cursor, 1.0 / 60.0, &[], true);
        }
        assert!(sculpture.objects.held_stack().unwrap().radii[0] > radius, "picked-up ball can gather more snow");
        let destination = V2::new(320.0, 150.0);
        sculpture.frame(&mut snow, &[], destination, 1.0 / 60.0, &[], true);
        sculpture.frame(&mut snow, &[], destination, 1.0 / 60.0, &[PointerEvent::Up(destination)], true);
        assert!((sculpture.objects.stacks()[0].base.x - destination.x).abs() < 1.0, "release does not throw the ball");
    }

    #[test]
    fn released_ball_can_be_placed_on_another() {
        let mut sculpture = blank();
        let mut snow = SnowSimulation::new(500.0, 400.0, 0.0);
        sculpture.active = true;
        assert!(sculpture.objects.restore(V2::new(100.0, 0.0), vec![12.0], Some((0, 100.0))));
        assert!(sculpture.objects.restore(V2::new(180.0, 0.0), vec![8.0], Some((0, 180.0))));
        let centre = V2::new(180.0, 8.0);
        sculpture.frame(&mut snow, &[], centre, 0.016, &[PointerEvent::Down(centre)], true);
        let destination = V2::new(100.0, 32.0);
        sculpture.frame(&mut snow, &[], destination, 0.016, &[], true);
        sculpture.frame(&mut snow, &[], destination, 0.016, &[PointerEvent::Up(destination)], true);
        assert_eq!(sculpture.objects.stacks().len(), 1);
        assert_eq!(sculpture.objects.stacks()[0].radii, vec![12.0, 8.0]);
    }

    #[test]
    fn dragged_ball_released_above_a_snowman_lands_on_top() {
        let mut sculpture = blank();
        let mut snow = SnowSimulation::new(500.0, 400.0, 0.0);
        assert!(sculpture.objects.restore(V2::new(100.0, 0.0), vec![15.0, 10.0, 6.0], Some((0, 100.0))));
        assert!(sculpture.objects.restore(V2::new(200.0, 0.0), vec![7.0], Some((0, 200.0))));
        let top = sculpture.objects.stacks()[0].height();
        let from = V2::new(200.0, 7.0);
        let above = V2::new(100.0, top + 32.0);
        sculpture.frame(&mut snow, &[], from, 0.016, &[PointerEvent::Down(from)], true);
        assert!(sculpture.objects.holding());
        sculpture.frame(&mut snow, &[], above, 0.016, &[], true);
        for _ in 0..60 {
            sculpture.frame(&mut snow, &[], above, 1.0 / 60.0, &[], true);
        }
        assert!(sculpture.objects.holding(), "hovering over a snowman must not release or break the ball");
        sculpture.frame(&mut snow, &[], above, 0.016, &[PointerEvent::Up(above)], true);
        for _ in 0..180 {
            sculpture.frame(&mut snow, &[], above, 1.0 / 60.0, &[], true);
        }
        assert_eq!(sculpture.objects.stacks().len(), 1);
        assert_eq!(sculpture.objects.stacks()[0].radii, vec![15.0, 10.0, 6.0, 7.0]);
        assert!(sculpture.objects.stacks()[0].snowman());
        assert!(sculpture.sprites(&[], &snow)[0].sprites.len() > 7);
    }

    #[test]
    fn released_ball_remains_visible_and_pickable_over_a_window() {
        let mut sculpture = blank();
        let mut snow = SnowSimulation::new(500.0, 400.0, 0.0);
        snow.covers.push([0.0, 0.0, 500.0, 400.0]);
        sculpture.active = true;
        let centre = V2::new(100.0, 100.0);
        assert!(sculpture.objects.create(centre, 10.0));
        assert!(sculpture.sprites(&[], &snow)[0].covers.is_empty());
        sculpture.frame(&mut snow, &[], centre, 0.016, &[PointerEvent::Up(centre)], true);
        assert!(sculpture.sprites(&[], &snow)[0].covers.is_empty(), "releasing the ball must not hide it behind the window");
        sculpture.frame(&mut snow, &[], centre, 0.016, &[PointerEvent::Exit], true);
        assert!(sculpture.sprites(&[], &snow)[0].covers.is_empty(), "the ball stays visible after leaving sculpting mode");
        sculpture.active = true;
        let ball = &sculpture.objects.stacks()[0];
        let at = V2::new(ball.base.x, ball.base.y + ball.radii[0]);
        sculpture.frame(&mut snow, &[], at, 0.016, &[PointerEvent::Down(at)], true);
        assert!(sculpture.objects.holding(), "the visible released ball must be pickable");
    }

    #[test]
    #[ignore = "requires write access to the current user's Windows registry"]
    fn released_ball_is_saved_and_restored_from_windows_preferences() {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW};
        use windows::core::{PCWSTR, w};

        const TEST_DISPLAY: &str = "SNOWHUSH_SCULPT_TEST_DISPLAY";
        if let Ok(display) = std::env::var(TEST_DISPLAY) {
            let mut sculpture = Sculpt::new(&display, 500.0, 400.0);
            sculpture.active = true;
            assert!(sculpture.objects.create(V2::new(120.0, 45.0), 9.0));
            let mut snow = SnowSimulation::new(500.0, 400.0, 0.0);
            let at = V2::new(120.0, 45.0);
            sculpture.frame(&mut snow, &[], at, 0.016, &[PointerEvent::Up(at)], true);
            return;
        }

        let display = format!("sculpt-test-{}", std::process::id());
        let name: Vec<u16> = format!("sculptures:{display}").encode_utf16().chain([0]).collect();
        assert!(prefs::set_sculptures(&display, "snowhush-sculptures-1\n"), "test registry value must be writable");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["sculpt::tests::released_ball_is_saved_and_restored_from_windows_preferences", "--exact", "--ignored"])
            .env(TEST_DISPLAY, &display)
            .output()
            .unwrap();
        let mut restored = Sculpt::new(&display, 500.0, 400.0);
        let data = restored.objects.stacks().to_vec();
        let mut covered_snow = SnowSimulation::new(500.0, 400.0, 0.0);
        covered_snow.covers.push([0.0, 0.0, 500.0, 400.0]);
        let visible_after_restart = restored.sprites(&[], &covered_snow).first().is_some_and(|group| group.covers.is_empty());
        let centre = restored.objects.stacks()[0].ball_centres().next().unwrap().0;
        restored.frame(&mut covered_snow, &[], centre, 0.016, &[PointerEvent::Down(centre)], true);
        let high = V2::new(centre.x, 300.0);
        restored.frame(&mut covered_snow, &[], high, 0.016, &[], true);
        restored.frame(&mut covered_snow, &[], high, 0.016, &[PointerEvent::Up(high)], true);
        for _ in 0..180 {
            restored.frame(&mut covered_snow, &[], high, 1.0 / 60.0, &[], true);
        }
        drop(restored);
        let after_break = Sculpt::new(&display, 500.0, 400.0);
        let broken_stays_removed = after_break.objects.stacks().is_empty();
        drop(after_break);
        let removed = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, w!("Software\\VelvetFrost"), PCWSTR(name.as_ptr())) };
        assert_eq!(removed, windows::Win32::Foundation::ERROR_SUCCESS);
        assert!(child.status.success(), "saving process failed: {}", String::from_utf8_lossy(&child.stdout));
        assert!(visible_after_restart, "the restored ball stays visible over a window");
        assert!(broken_stays_removed, "a broken ball must not reappear after restart");
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].radii, vec![9.0]);
        assert!((data[0].base.x - 120.0).abs() < 0.01);
        assert!((data[0].base.y - 36.0).abs() < 0.5);
    }

    #[test]
    #[ignore = "requires write access to the current user's Windows registry"]
    fn a_snowman_with_an_extra_ball_is_saved_and_restored() {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW};
        use windows::core::{PCWSTR, w};

        let display = format!("sculpt-stack-test-{}", std::process::id());
        let name: Vec<u16> = format!("sculptures:{display}").encode_utf16().chain([0]).collect();
        assert!(prefs::set_sculptures(&display, "snowhush-sculptures-1\n"));
        let mut sculpture = Sculpt::new(&display, 500.0, 400.0);
        assert!(sculpture.objects.restore(V2::new(100.0, 0.0), vec![15.0, 10.0, 6.0], Some((0, 100.0))));
        let original_appearance = sculpture.objects.stacks()[0].appearance;
        let top = sculpture.objects.stacks()[0].height();
        let at = V2::new(100.0, top + 7.0);
        assert!(sculpture.objects.create(at, 7.0));
        let mut snow = SnowSimulation::new(500.0, 400.0, 0.0);
        sculpture.frame(&mut snow, &[], at, 0.016, &[PointerEvent::Up(at)], true);
        let original = sculpture_sprites_with_style(&sculpture.objects.stacks()[0], 0.0, false);
        drop(sculpture);
        let restored = Sculpt::new(&display, 500.0, 400.0);
        let radii = restored.objects.stacks().first().map(|stack| stack.radii.clone());
        let appearance = restored.objects.stacks().first().map(|stack| stack.appearance);
        let restored_sprites = restored.sprites(&[], &snow).remove(0).sprites;
        drop(restored);
        let removed = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, w!("Software\\VelvetFrost"), PCWSTR(name.as_ptr())) };
        assert_eq!(removed, windows::Win32::Foundation::ERROR_SUCCESS);
        assert_eq!(radii, Some(vec![15.0, 10.0, 6.0, 7.0]));
        assert_eq!(appearance, Some(original_appearance));
        assert_eq!(original.len(), restored_sprites.len());
        for (a, b) in original.iter().zip(&restored_sprites) {
            assert_eq!((a.position, a.size, a.tint, a.mood, a.pose), (b.position, b.size, b.tint, b.mood, b.pose));
        }
        assert!(restored_sprites.iter().any(|sprite| sprite.mood[0] == 5.0));
    }

    #[test]
    fn a_snowman_on_a_window_hides_with_its_support_and_uses_its_occluders() {
        let mut sculpture = blank();
        let snow = SnowSimulation::new(500.0, 400.0, 0.0);
        assert!(sculpture.objects.restore(V2::new(100.0, 200.0), vec![12.0, 8.0, 5.0], Some((7, 50.0))));
        assert!(sculpture.sprites(&[], &snow).is_empty());
        let cover = Rect { x: 90.0, y: 210.0, width: 20.0, height: 30.0 };
        let window = WindowSurface {
            id: 7,
            frame: Rect { x: 50.0, y: 100.0, width: 200.0, height: 100.0 },
            blockers: Vec::new(),
            cover: vec![cover],
            corner_radius: 0.0,
            corner_exponent: 2.0,
        };
        let groups = sculpture.sprites(&[window], &snow);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].covers, vec![[90.0, 210.0, 110.0, 240.0]]);
    }

    #[test]
    fn restored_windows_are_matched_only_when_unambiguous() {
        let first =
            WindowSurface { id: 10, frame: Rect::default(), blockers: Vec::new(), cover: Vec::new(), corner_radius: 0.0, corner_exponent: 2.0 };
        let second = WindowSurface { id: 11, ..first.clone() };
        let saved = "app.exe\nclass\nold title";
        let changed_title = vec![(&first, "app.exe\nclass\nnew title".to_string())];
        assert_eq!(matching_window(saved, true, &changed_title).unwrap().0.id, 10);
        assert!(matching_window(saved, false, &changed_title).is_none());
        let ambiguous = vec![(&first, "app.exe\nclass\nnew title".to_string()), (&second, "app.exe\nclass\nanother".to_string())];
        assert!(matching_window(saved, true, &ambiguous).is_none());
        let exact = vec![(&first, saved.to_string()), (&second, "app.exe\nclass\nanother".to_string())];
        assert_eq!(matching_window(saved, false, &exact).unwrap().0.id, 10);
    }
}
