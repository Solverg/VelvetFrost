use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Once;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::SetEvent;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_NOREPEAT, RegisterHotKey, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, UnregisterHotKey, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, Error, PCWSTR, Result, w};

use snowhush_core::geometry::V2;
use snowhush_core::halloween::moonlit;
use snowhush_core::simulation::{Mode, Pace, SnowSimulation, Sprite, WindowId, WindowSurface};
use snowhush_core::snowmen::Stack;

use crate::bullfinch::{self, Bullfinch};
use crate::cone::{Cone, HIT_RADIUS};
use crate::desktop::DesktopLayer;
use crate::fox::{self, Fox};
use crate::light::{Backdrop, Sky, Wallpapers};
use crate::log;
use crate::plow::Plow;
use crate::prefs::Settings;
use crate::render::{Ambient, Gpu, LANTERN_REACH, Surface};
use crate::sculpt::{PointerEvent, Sculpt, SculptureDraw, cone_sprites, tree_radius};
use crate::sleigh::Sleigh;
use crate::tracker::{COVER_ONLY, Tracker, class_name, cloaked, local_frame, process_name};
use crate::tree_space::{self, Occupant};
use crate::truck::Truck;

const CLASS: PCWSTR = w!("SnowhushOverlay");
const SCULPT_ESCAPE: i32 = 0x534e;
/// Seconds the snow takes to fade out of a full-screen app's way, and back in once it goes.
const FADE_OUT: f32 = 0.25;
const FADE_IN: f32 = 0.35;

fn fox_has_support(simulation: &SnowSimulation, x: f32) -> bool {
    if !simulation.modes().contains(&Mode::Snow) {
        return false;
    }
    let area = simulation.snow_available(0, x, 15.0);
    if area < fox::BURROW_SNOW {
        return false;
    }
    let samples = fox::SUPPORT_OFFSETS.map(|offset| {
        let point = x + offset;
        (simulation.ground_surface(point), simulation.snow_available(0, point, 6.0))
    });
    fox::suitable_drift(area, samples)
}

fn bullfinch_has_support(simulation: &SnowSimulation, surfaces: &[WindowSurface], x: f32) -> bool {
    if !simulation.modes().contains(&Mode::Snow)
        || x < bullfinch::SIZE * 0.5
        || x > simulation.width() - bullfinch::SIZE * 0.5
        || simulation.snow_available(0, x, 10.0) < 10.0
    {
        return false;
    }
    let ground = simulation.ground_surface(x);
    !surfaces.iter().any(|window| {
        x + bullfinch::SIZE * 0.35 > window.frame.min_x()
            && x - bullfinch::SIZE * 0.35 < window.frame.max_x()
            && ground < window.frame.max_y()
            && ground + bullfinch::SIZE * 0.6 > window.frame.min_y()
    })
}

thread_local! {
    /// Set by an overlay window when the displays change under it.
    pub static DISPLAYS_CHANGED: Cell<bool> = const { Cell::new(false) };
    /// Set by an overlay window when the desktop picture or colour changes.
    pub static BACKDROP_CHANGED: Cell<bool> = const { Cell::new(false) };
    /// The windows at the top last logged by `lowest_open_popup`.
    static TOP_LOGGED: RefCell<String> = const { RefCell::new(String::new()) };
    static SCULPT_INPUT: RefCell<HashMap<isize, SculptInput>> = RefCell::new(HashMap::new());
    static SCULPT_ACTIVE: Cell<isize> = const { Cell::new(0) };
}

#[derive(Default)]
struct SculptInput {
    captures: bool,
    active: bool,
    is_cone: bool,
    hovered: bool,
    icon: Option<RECT>,
    balls: Vec<RECT>,
    hotkey_attempted: bool,
    hotkey_registered: bool,
    events: Vec<(bool, Option<POINT>)>,
}

struct GiftDrag {
    index: usize,
    start: V2,
    offset: V2,
    moved: bool,
    original: crate::gifts::Gift,
}

/// How much the snow may spend on drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerBudget {
    Full,
    /// On battery: no frame rates above 60, which are hard to tell apart in snow anyway.
    Battery,
    /// Energy saver: a little less snow, drawn a little less often.
    Saver,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Monitor {
    pub name: String,
    /// The work area in physical pixels: the taskbar is the ground the snow settles on, so it is left out,
    /// unless the snow goes behind the taskbar, or rain runs down it, which it then reaches down past to the bottom of
    /// the display; the whole display while a full-screen app the snow shows over has put the taskbar away.
    pub rect: RECT,
    /// The whole display in physical pixels.
    pub bounds: RECT,
    pub dpi: u32,
    pub refresh: u32,
}

pub fn monitors(over_taskbar: bool, full_screen: &HashSet<String>) -> Vec<Monitor> {
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
        let list = unsafe { &mut *(list.0 as *mut Vec<HMONITOR>) };
        list.push(monitor);
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&raw mut handles as isize));
    }
    handles
        .into_iter()
        .filter_map(|handle| unsafe {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if !GetMonitorInfoW(handle, (&raw mut info).cast::<MONITORINFO>()).as_bool() {
                return None;
            }
            let (mut dpi, mut _y) = (96, 96);
            let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut _y);
            let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
            let refresh = if EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool() {
                mode.dmDisplayFrequency
            } else {
                60
            };
            let length = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            let (work, bounds) = (info.monitorInfo.rcWork, info.monitorInfo.rcMonitor);
            let name = String::from_utf16_lossy(&info.szDevice[..length]);
            let rect = if full_screen.contains(&name) {
                bounds
            } else if over_taskbar {
                RECT { bottom: bounds.bottom, ..work }
            } else {
                work
            };
            Some(Monitor { name, rect, bounds: info.monitorInfo.rcMonitor, dpi, refresh })
        })
        .collect()
}

/// Which side edges of a display's work area, left and right, have no display past them, so snow heaps up against them.
pub fn walls(monitor: &Monitor, displays: &[Monitor]) -> (bool, bool) {
    let (area, bounds) = (monitor.rect, monitor.bounds);
    let beside = |edge: fn(&RECT) -> i32, x: i32| {
        displays
            .iter()
            .any(|other| other.bounds != bounds && other.bounds.top < bounds.bottom && other.bounds.bottom > bounds.top && edge(&other.bounds) == x)
    };
    // A taskbar along a side stands between the snow and any display past it.
    (area.left != bounds.left || !beside(|r| r.right, bounds.left), area.right != bounds.right || !beside(|r| r.left, bounds.right))
}

/// The display's vertical blanks on the performance counter: frames are started just after one, so they keep pace with it.
pub struct Clock {
    pub frequency: i64,
    period: i64,
    phase: i64,
    hz: f64,
}

impl Clock {
    pub fn new() -> Clock {
        let mut frequency = 0;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut frequency);
        }
        let mut clock = Clock { frequency, period: frequency / 60, phase: 0, hz: 60.0 };
        clock.refresh();
        clock
    }

    pub fn now() -> i64 {
        let mut now = 0;
        unsafe {
            let _ = QueryPerformanceCounter(&mut now);
        }
        now
    }

    pub fn refresh(&mut self) {
        let mut info = DWM_TIMING_INFO { cbSize: size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
        if unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }.is_err() || info.qpcRefreshPeriod == 0 {
            return;
        }
        self.period = info.qpcRefreshPeriod as i64;
        self.phase = info.qpcVBlank as i64;
        let ratio = info.rateRefresh;
        self.hz = if ratio.uiDenominator > 0 { ratio.uiNumerator as f64 / ratio.uiDenominator as f64 } else { 60.0 };
    }

    /// When to start the frame after one started at `now`, at `rate` frames per second.
    fn schedule(&self, now: i64, rate: u32) -> i64 {
        let divisor = (self.hz / rate as f64).round().max(1.0) as i64;
        let earliest = now + divisor * self.period - self.period / 2;
        let vblanks = (earliest - self.phase + self.period - 1).div_euclid(self.period);
        self.phase + vblanks * self.period + self.frequency / 2000
    }

    pub fn seconds(&self, ticks: i64) -> f64 {
        ticks as f64 / self.frequency as f64
    }

    pub fn tolerance(&self) -> i64 {
        self.period / 4
    }
}

struct Overlay(HWND);

/// A hidden top-level window for the system's broadcasts about displays and the wallpaper,
/// which overlays behind windows, as child windows, never get.
pub struct Listener {
    _window: Overlay,
}

pub fn listen() -> Result<Listener> {
    register_class()?;
    unsafe {
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("VelvetFrost"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        Ok(Listener { _window: Overlay(window) })
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        SCULPT_INPUT.with_borrow_mut(|input| {
            input.remove(&(self.0.0 as isize));
        });
        if SCULPT_ACTIVE.get() == self.0.0 as isize {
            SCULPT_ACTIVE.set(0);
        }
        unsafe {
            let _ = UnregisterHotKey(Some(self.0), SCULPT_ESCAPE);
            let _ = DestroyWindow(self.0);
        }
    }
}

/// The snow on one display: its overlay window, drawing surface and simulation.
pub struct Screen {
    pub monitor: Monitor,
    pub simulation: SnowSimulation,
    pub next_due: i64,
    pub plow: Option<Plow>,
    pub truck: Option<Truck>,
    pub sleigh: Option<Sleigh>,
    vehicle_dirty: bool,
    surface: Surface,
    overlay: Overlay,
    /// Trees have their own window so their stacking does not change the snow's stacking.
    tree_overlay: Option<(Overlay, Surface, Option<DesktopLayer>)>,
    trees_on_top: bool,
    snow_behind_requested: bool,
    trees_before_snowmen: bool,
    tree_dirty: bool,
    /// When snow is behind windows, a separate transparent topmost surface receives sculpting input.
    control: Option<(Overlay, Surface)>,
    control_failed: bool,
    shown_in_screenshots: bool,
    sculpt: Sculpt,
    trees: Sculpt,
    cone: Option<Cone>,
    star_chain: Option<crate::star_event::StarChain>,
    fox: Option<Fox>,
    bullfinch: Option<Bullfinch>,
    gifts: Vec<crate::gifts::Gift>,
    collection_undo: Option<CollectionUndo>,
    gift_pressed: bool,
    gift_drag: Option<GiftDrag>,
    gifts_saved: String,
    gifts_writable: bool,
    gifts_save_delay: f32,
    /// A small independent input window follows the moving cone even before the pointer approaches it.
    cone_input: Option<Overlay>,
    cone_input_failed: bool,
    tree_origins: HashMap<u64, (V2, Option<(isize, f32)>)>,
    size: (u32, u32),
    last_step: i64,
    idle_frames: u32,
    interactive_rate: u32,
    /// Every frame at `interactive_rate`, whatever the snow is doing and whatever the power.
    pub max_rate: bool,
    surfaces: Vec<WindowSurface>,
    /// The taskbar's ledge while it rains, as last seen: one that hides itself slides in and out with no window
    /// changing, so it is looked at every frame.
    taskbar: Option<WindowSurface>,
    /// Whether the simulation has had this overlay's windows at least once: one made anew for a change of settings
    /// carries on a simulation with the old overlay's, which might be gone, as the taskbar's are behind it.
    synced: bool,
    windows_seen: u64,
    windows_changed_at: i64,
    covered: bool,
    /// How much of the snow shows, from 0 to 1.
    opacity: f32,
    backdrop: Option<Backdrop>,
    shadow_tint: [f32; 4],
    layer: Option<DesktopLayer>,
    behind_taskbar: bool,
    /// Where the cursor's candlelight was last drawn.
    lantern_at: V2,
}

enum CollectionUndo {
    Tree { id: u64, base: V2, support: (isize, f32), gifts: Vec<crate::gifts::Gift> },
    Gift(crate::gifts::Gift),
}

impl Screen {
    /// `layer` puts the snow behind windows, in the desktop's wallpaper layer; without it the snow is on top of everything.
    /// On failure the simulation comes back, so its snow can go elsewhere.
    pub fn new(
        gpu: &Gpu,
        monitor: Monitor,
        mut simulation: SnowSimulation,
        settings: &Settings,
        layer: Option<DesktopLayer>,
    ) -> std::result::Result<Screen, (Error, SnowSimulation)> {
        let size = dip_size(&monitor);
        let (overlay, surface) = match Self::overlay(gpu, &monitor, size, settings.show_in_screenshots, layer) {
            Ok(parts) => parts,
            Err(error) => return Err((error, simulation)),
        };
        simulation.set_external_ghost_spawning(true);
        let sculpt = Sculpt::new(&monitor.name, size.0 as f32, size.1 as f32);
        let trees = Sculpt::trees(&monitor.name, size.0 as f32, size.1 as f32);
        let stored = crate::prefs::sculptures(&format!("boomboxes:{}", monitor.name));
        let raw = stored.as_ref().ok().and_then(|value| value.clone()).unwrap_or_else(|| "snowhush-boomboxes-1\n".to_string());
        let decoded = crate::gifts::decode_saved(&raw);
        let gifts_writable = stored.is_ok() && decoded.is_some();
        let gifts = decoded
            .unwrap_or_default()
            .into_iter()
            .map(|saved| {
                let mut gift = crate::gifts::Gift::with_kind(
                    saved.kind,
                    crate::gifts::GiftColor::Red,
                    saved.x * size.0 as f32,
                    saved.y * size.1 as f32,
                    saved.size,
                    0,
                    0.0,
                );
                gift.enabled = saved.enabled;
                gift.revealed = true;
                gift.age = 0.75;
                match saved.support {
                    crate::gifts::SavedSupport::Ground => (),
                    crate::gifts::SavedSupport::Falling => gift.begin_fall(),
                    crate::gifts::SavedSupport::Stored => gift.stored = true,
                    crate::gifts::SavedSupport::Tree(id, slot) => gift.attachment = Some((id, slot)),
                    crate::gifts::SavedSupport::Window(key, fraction) => gift.pending_window = Some((key, fraction)),
                }
                gift
            })
            .collect();
        if !gifts_writable {
            log!("could not read boomboxes on {}; leaving saved data unchanged", monitor.name);
        }
        Ok(Screen {
            interactive_rate: monitor.refresh.clamp(60, 120),
            max_rate: settings.max_performance,
            monitor,
            simulation,
            next_due: 0,
            plow: None,
            truck: None,
            sleigh: None,
            vehicle_dirty: false,
            surface,
            overlay,
            tree_overlay: None,
            trees_on_top: settings.trees_on_top,
            snow_behind_requested: settings.behind_windows,
            trees_before_snowmen: settings.trees_before_snowmen,
            tree_dirty: true,
            control: None,
            control_failed: false,
            shown_in_screenshots: settings.show_in_screenshots,
            sculpt,
            trees,
            cone: None,
            star_chain: None,
            fox: None,
            bullfinch: None,
            gifts,
            collection_undo: None,
            gift_pressed: false,
            gift_drag: None,
            gifts_saved: raw,
            gifts_writable,
            gifts_save_delay: 0.0,
            cone_input: None,
            cone_input_failed: false,
            tree_origins: HashMap::new(),
            size,
            last_step: 0,
            idle_frames: 0,
            surfaces: Vec::new(),
            taskbar: None,
            synced: false,
            windows_seen: 0,
            windows_changed_at: 0,
            covered: false,
            opacity: 1.0,
            backdrop: None,
            shadow_tint: [0.0; 4],
            layer,
            behind_taskbar: settings.behind_taskbar,
            lantern_at: V2::new(-10_000.0, -10_000.0),
        })
        .map(|mut screen| {
            screen.set_trees_on_top(gpu, settings.trees_on_top, 0);
            screen
        })
    }

    pub fn set_trees_on_top(&mut self, gpu: &Gpu, on: bool, now: i64) {
        self.tree_overlay = None;
        self.trees_on_top = on;
        // When both kinds belong in the same window layer, render them together.
        if on || self.snow_behind_requested {
            self.tree_dirty = true;
            self.wake(now);
            return;
        }
        if let Some(layer) = crate::desktop::find() {
            match Self::overlay(gpu, &self.monitor, self.size, self.shown_in_screenshots, Some(layer)) {
                Ok((overlay, surface)) => {
                    let _ = surface.set_opacity(gpu, self.opacity);
                    self.tree_overlay = Some((overlay, surface, Some(layer)));
                }
                Err(error) => log!("tree overlay unavailable on {}: {error}", self.monitor.name),
            }
        }
        self.tree_dirty = true;
        self.wake(now);
    }

    pub fn tree_overlay_missing(&self) -> bool {
        !self.trees_on_top
            && !self.snow_behind_requested
            && self.tree_overlay.as_ref().is_none_or(|(overlay, _, _)| unsafe { !IsWindow(Some(overlay.0)).as_bool() })
    }

    pub fn set_trees_before_snowmen(&mut self, on: bool, now: i64) {
        self.trees_before_snowmen = on;
        self.tree_dirty = true;
        self.wake(now);
    }

    fn overlay(
        gpu: &Gpu,
        monitor: &Monitor,
        size: (u32, u32),
        shown_in_screenshots: bool,
        layer: Option<DesktopLayer>,
    ) -> Result<(Overlay, Surface)> {
        let overlay = Overlay(match layer {
            Some(layer) => create_behind(&monitor.rect, &layer)?,
            None => create_overlay(&monitor.rect, shown_in_screenshots)?,
        });
        let (physical_width, physical_height) = physical_size(monitor);
        let scale = (physical_width as f32 / size.0 as f32, physical_height as f32 / size.1 as f32);
        let surface = Surface::new(gpu, overlay.0, size.0, size.1, scale)?;
        Ok((overlay, surface))
    }

    /// Steps the snow and draws it if anything visible changed; returns whether it drew.
    /// Track sculpture supports even when snow on windows is disabled.
    pub fn frame(
        &mut self,
        gpu: &Gpu,
        clock: &Clock,
        now: i64,
        budget: PowerBudget,
        sky: Sky,
        time: f32,
        tracker: &Tracker,
        snow_on_windows: bool,
    ) -> Result<bool> {
        let dt = if self.last_step == 0 { 1.0 / 60.0 } else { clock.seconds(now - self.last_step).min(0.05) as f32 };
        self.last_step = now;
        self.follow(tracker, clock, now, snow_on_windows);
        let cursor = self.cursor();
        let tree_cursor = if self.tree_overlay_missing()
            || (!self.trees_on_top
                && !self.trees.objects.holding()
                && tracker
                    .covers(&self.monitor.rect, self.size)
                    .iter()
                    .any(|r| cursor.x >= r[0] && cursor.x <= r[2] && cursor.y >= r[1] && cursor.y <= r[3]))
        {
            V2::new(-10_000.0, -10_000.0)
        } else {
            cursor
        };
        let input_window = self.interaction_window();
        let input = SCULPT_INPUT
            .with_borrow_mut(|inputs| inputs.get_mut(&(input_window.0 as isize)).map(|input| std::mem::take(&mut input.events)).unwrap_or_default());
        let events: Vec<(PointerEvent, Option<POINT>)> = input
            .into_iter()
            .map(|(down, point)| {
                let event = match point {
                    Some(point) if down => PointerEvent::Down(self.local(point.x as f32, point.y as f32)),
                    Some(point) => PointerEvent::Up(self.local(point.x as f32, point.y as f32)),
                    None if down => PointerEvent::Exit,
                    None => PointerEvent::Cancel,
                };
                (event, point)
            })
            .collect();
        let allowed = !self.control_failed;
        let mut sculpt_events = Vec::new();
        let mut tree_events = Vec::new();
        let mut route_tree = self.trees.objects.holding();
        let mut tree_drop = false;
        let mut cone_changed = false;
        if let Some(window) = &self.cone_input {
            if self.star_chain.is_some() {
                let clicked = take_collectible_click(window.0);
                if clicked && !self.sculpt.objects.holding() && !route_tree {
                    self.catch_star();
                    cone_changed = true;
                }
            } else {
                cone_changed = collect_cone_click(window.0, &mut self.cone, self.sculpt.objects.holding() || route_tree);
            }
        }
        for (event, screen_point) in events {
            match event {
                PointerEvent::Down(at)
                    if !self.sculpt.objects.holding() && !route_tree && self.star_chain.as_ref().is_some_and(|star| star.hit(at)) =>
                {
                    self.catch_star();
                    cone_changed = true;
                }
                PointerEvent::Down(at)
                    if !self.sculpt.objects.holding() && !route_tree && self.gifts.iter().any(|gift| self.gift_visible(gift) && gift.hit(at)) =>
                {
                    self.gift_pressed = true;
                    if let Some(index) = self.gifts.iter().rposition(|gift| self.gift_visible(gift) && gift.hit(at)) {
                        let gift = &mut self.gifts[index];
                        if gift.revealed {
                            self.gift_drag =
                                Some(GiftDrag { index, start: at, offset: V2::new(gift.x, gift.bottom) - at, moved: false, original: gift.clone() });
                        } else {
                            gift.open();
                        }
                    }
                }
                PointerEvent::Up(at) if self.gift_pressed => {
                    self.move_gift_drag(at);
                    if self.gift_drag.as_ref().is_some_and(|drag| drag.moved) && screen_point.is_some_and(crate::inventory::drop_target_at) {
                        let result = self.store_dragged_gift();
                        crate::inventory::report_drop(self.monitor.name.clone(), result);
                    } else {
                        self.release_gift_drag(true);
                        self.gift_pressed = false;
                    }
                }
                PointerEvent::Down(at) if !self.sculpt.objects.holding() && !route_tree && self.cone.as_ref().is_some_and(|cone| cone.hit(at)) => {
                    if let Some(cone) = &mut self.cone {
                        cone.collected = true;
                        cone_changed = true;
                    }
                }
                PointerEvent::Down(at)
                    if !self.sculpt.objects.holding()
                        && self.trees.captures(at)
                        && (self.trees_before_snowmen && self.trees_on_top || !self.sculpt.captures(at)) =>
                {
                    route_tree = true;
                    tree_events.push(event);
                }
                PointerEvent::Cancel | PointerEvent::Exit => {
                    self.release_gift_drag(false);
                    self.gift_pressed = false;
                    sculpt_events.push(event);
                    // ReleaseCapture can queue cancellation immediately after the mouse-up;
                    // keep the tree held until the pending collection drop is committed.
                    if !tree_drop {
                        tree_events.push(event);
                    }
                }
                PointerEvent::Up(_) if route_tree && screen_point.is_some_and(crate::inventory::drop_target_at) => {
                    tree_drop = true;
                }
                _ if route_tree => tree_events.push(event),
                _ => sculpt_events.push(event),
            }
        }
        self.move_gift_drag(cursor);
        if !self.simulation.modes().contains(&Mode::Snow) {
            self.star_chain = None;
        }
        if let Some(star) = &mut self.star_chain {
            cone_changed = true;
            if !star.advance(dt) {
                self.star_chain = None;
            }
        }
        if let Some(mut cone) = self.cone.take() {
            let (_, current) = cone.advance(dt);
            cone_changed = true;
            let mut finished = false;
            if cone.collected && !cone.planted {
                let x = cone.position.x;
                let ground = self.simulation.ground_surface(x);
                let mut landings = Vec::new();
                if ground <= current && self.simulation.snow_available(0, x, 4.0) > 0.0 {
                    landings.push((0, 0.0, ground));
                }
                for window in &self.surfaces {
                    if x < window.frame.min_x() || x > window.frame.max_x() {
                        continue;
                    }
                    if let Some(y) = self.simulation.snow_surface_on(window.id, x)
                        && self.simulation.snow_available(window.id, x, 4.0) > 0.0
                        && y <= current
                    {
                        landings.push((window.id, window.frame.min_x(), y));
                    }
                }
                landings.sort_by(|a, b| b.2.total_cmp(&a.2));
                let height = if cone.legendary.is_some() { crate::sculpt::celestial_height(cone.random) } else { tree_radius(cone.random) * 5.4 };
                for (id, left, y) in landings {
                    if let Some(free_x) = self.free_tree_x(x, id, y, height, None) {
                        let actual_y =
                            if id == 0 { Some(self.simulation.ground_surface(free_x)) } else { self.simulation.snow_surface_on(id, free_x) };
                        let Some(actual_y) = actual_y else { continue };
                        if actual_y > current || self.simulation.snow_available(id, free_x, 4.0) <= 0.0 {
                            continue;
                        }
                        if self.free_tree_x(free_x, id, actual_y, height, None) != Some(free_x) {
                            continue;
                        }
                        // Do not plant through a different window covering the selected ledge.
                        if self.surfaces.iter().any(|window| {
                            window.id != id
                                && free_x + height * 0.375 > window.frame.min_x()
                                && free_x - height * 0.375 < window.frame.max_x()
                                && actual_y < window.frame.max_y()
                                && actual_y + height > window.frame.min_y()
                        }) {
                            continue;
                        }
                        let planted = if let Some(variant) = cone.legendary {
                            let owner_key = crate::star_event::owner_key(variant);
                            if crate::prefs::set_sculptures(owner_key, &self.monitor.name) {
                                let planted = self.trees.plant_legendary(V2::new(free_x, actual_y), (id, free_x - left), cone.random, variant);
                                if planted {
                                    self.trees.persist_now();
                                } else {
                                    let _ = crate::prefs::set_sculptures(owner_key, "");
                                }
                                planted
                            } else {
                                false
                            }
                        } else {
                            self.trees.plant(V2::new(free_x, actual_y), (id, free_x - left), cone.random, cone.species)
                        };
                        if planted {
                            cone.planted = true;
                            break;
                        }
                    }
                }
            } else if !cone.collected && current <= 0.0 {
                finished = true;
            }
            if cone.planted && cone.collection_time >= 0.35 {
                finished = true;
            }
            if !finished {
                self.cone = Some(cone);
            }
        }
        let previous_trees = self.trees.objects.stacks().to_vec();
        let mut sculpture_changed = self.sculpt.frame(&mut self.simulation, &self.surfaces, cursor, dt, &sculpt_events, allowed);
        sculpture_changed |= self.trees.frame(&mut self.simulation, &self.surfaces, tree_cursor, dt, &tree_events, allowed) || cone_changed;
        if let Some(held) = self.trees.objects.held_stack()
            && let Some(previous) = previous_trees.iter().find(|stack| stack.appearance == held.appearance)
        {
            self.tree_origins.entry(held.appearance).or_insert((previous.base, previous.support));
        }
        if tree_drop {
            let result = self.store_dragged_tree();
            crate::inventory::report_drop(self.monitor.name.clone(), result);
            sculpture_changed = true;
        }
        sculpture_changed |= self.resolve_tree_placements(&previous_trees);
        for (index, gift) in self.gifts.iter_mut().enumerate() {
            if gift.stored {
                continue;
            }
            if gift.size <= 0.0 {
                continue;
            }
            if let Some(age) = &mut gift.broken {
                *age += dt.min(0.05);
                if *age >= 0.6 {
                    gift.size = 0.0;
                }
                sculpture_changed = true;
                continue;
            }
            if self.gift_drag.as_ref().is_some_and(|drag| drag.index == index && drag.moved) {
                sculpture_changed = true;
                continue;
            }
            if let Some((tree_id, slot_id)) = gift.attachment {
                if let Some(tree) = self.trees.objects.stacks().iter().find(|tree| tree.appearance == tree_id) {
                    let (species, variant) = self.trees.tree_style(tree_id);
                    if let Some(slot) = crate::ornaments::tree_slots(species, variant).into_iter().find(|slot| slot.id == slot_id) {
                        let centre = slot.world_position(tree.base, tree.height_from_radii());
                        let old = V2::new(gift.x, gift.bottom);
                        gift.x = centre.x;
                        gift.bottom = centre.y - gift.size * 1.5 * 0.30;
                        gift.tilt = 0.0;
                        gift.support = tree.support.map_or(0, |support| support.0);
                        sculpture_changed |= (V2::new(gift.x, gift.bottom) - old).length_squared() > 0.0001;
                        continue;
                    }
                } else if self.trees.knows_tree(tree_id) {
                    continue;
                }
                gift.begin_fall();
            }
            if let Some((key, fraction)) = &gift.pending_window {
                let mut matches = self.surfaces.iter().filter(|window| crate::sculpt::window_key(window.id).as_ref() == Some(key));
                let first = matches.next();
                if let Some(window) = first.filter(|_| matches.next().is_none()) {
                    gift.support = window.id;
                    gift.offset = fraction * window.frame.width;
                    gift.x = window.frame.min_x() + gift.offset;
                    gift.pending_window = None;
                    sculpture_changed = true;
                } else {
                    continue;
                }
            }
            if gift.support == -1 {
                let mut landing = (0, 0.0, self.simulation.ground_surface(gift.x));
                for window in &self.surfaces {
                    if gift.x >= window.frame.min_x() && gift.x <= window.frame.max_x() {
                        let y = self.simulation.snow_surface_on(window.id, gift.x).unwrap_or(window.frame.max_y());
                        if y <= gift.bottom + 0.1 && y > landing.2 {
                            landing = (window.id, window.frame.min_x(), y);
                        }
                    }
                }
                let (landed, broken) = gift.fall(dt, landing.2);
                sculpture_changed = true;
                if broken {
                    gift.broken = Some(0.0);
                } else if landed {
                    gift.support = landing.0;
                    gift.offset = gift.x - landing.1;
                }
                continue;
            }
            let old_x = gift.x;
            if gift.support != 0 {
                if let Some(window) = self.surfaces.iter().find(|window| window.id == gift.support) {
                    gift.x = window.frame.min_x() + gift.offset;
                } else if unsafe { IsWindow(Some(HWND(gift.support as _))).as_bool() } {
                    continue;
                } else {
                    gift.begin_fall();
                    sculpture_changed = true;
                    continue;
                }
            }
            let surface = |x: f32| {
                if gift.support == 0 {
                    self.simulation.ground_surface(x)
                } else {
                    self.simulation
                        .snow_surface_on(gift.support, x)
                        .or_else(|| self.surfaces.iter().find(|window| window.id == gift.support).map(|window| window.frame.max_y()))
                        .unwrap_or_else(|| self.simulation.ground_surface(x))
                }
            };
            let left = surface(gift.x - gift.size * 0.375);
            let right = surface(gift.x + gift.size * 0.375);
            sculpture_changed |= ((gift.x - old_x).abs() > 0.001) | gift.follow_terrain(dt, left, right) | gift.advance(dt);
        }
        // A lit lantern melts only snow on nearby visible surfaces, including a
        // ledge beneath a tree ornament; leaves and petals are left intact.
        if self.simulation.modes().contains(&Mode::Snow) && dt.is_finite() && dt > 0.0 {
            let lights: Vec<_> = self
                .gifts
                .iter()
                .filter(|gift| gift.is_lit_lantern() && self.gift_visible(gift))
                .map(|gift| V2::new(gift.x, gift.bottom + gift.size * 0.5))
                .collect();
            for light in lights {
                let surfaces = std::iter::once(0).chain(self.surfaces.iter().map(|window| window.id));
                for id in surfaces {
                    self.simulation.melt_snow_near(id, light, crate::gifts::LANTERN_RADIUS, crate::gifts::LANTERN_MELT_AREA_PER_SECOND, dt);
                }
                sculpture_changed = true; // Keep the warm light gently flickering.
            }
        }
        // No index can shift underneath a pointer capture; remove broken objects after release.
        if self.gift_drag.is_none() {
            self.gifts.retain(|gift| gift.size > 0.0);
        }
        self.gifts_save_delay -= dt;
        if self.gift_drag.is_none() && self.gifts_save_delay <= 0.0 {
            self.save_gifts();
            self.gifts_save_delay = 0.25;
        }
        self.sync_cone_input(tracker);
        let captures = self.sculpt.captures(cursor)
            || self.trees.captures(tree_cursor)
            || self.gifts.iter().any(|gift| self.gift_visible(gift) && gift.hit(if gift.attachment.is_some() { tree_cursor } else { cursor }))
            || (self.cone_input_failed
                && (self.cone.as_ref().is_some_and(|cone| cone.hit(cursor)) || self.star_chain.as_ref().is_some_and(|star| star.hit(cursor))));
        if self.layer.is_some() && self.control.is_none() && (captures || self.trees.has_icon()) && !self.control_failed {
            match Self::overlay(gpu, &self.monitor, self.size, self.shown_in_screenshots, None) {
                Ok(control) => {
                    self.control = Some(control);
                    sculpture_changed = true;
                }
                Err(error) => {
                    log!("sculpting overlay unavailable on {}: {error}", self.monitor.name);
                    self.control_failed = true;
                    self.sculpt.stop();
                }
            }
        }
        let interacting = self.sculpt.active() || self.sculpt.objects.holding() || self.trees.objects.holding() || self.gift_pressed;
        self.sculpt_capture(captures && !self.control_failed, interacting);
        if self.layer.is_some() && !self.sculpt.has_icon() && !self.trees.has_icon() && !interacting && !captures && self.control.is_some() {
            self.sculpt_capture(false, false);
            self.control = None;
            sculpture_changed = true;
        }
        let snow_cursor = if interacting { V2::new(-10_000.0, -10_000.0) } else { cursor };
        let changed = self.simulation.step(dt, snow_cursor) || sculpture_changed || self.tree_dirty;
        let fox_changed = self.fox.is_some();
        if let Some(fox) = &mut self.fox {
            let ground = self.simulation.ground_surface(fox.x);
            let simulation = &self.simulation;
            let surfaces = &self.surfaces;
            let can_step = |x: f32| {
                if x < fox::SIZE * 0.5 || x > simulation.width() - fox::SIZE * 0.5 || simulation.snow_available(0, x, 15.0) <= 0.0 {
                    return false;
                }
                let y = simulation.ground_surface(x);
                !surfaces.iter().any(|window| {
                    x + fox::SIZE * 0.35 > window.frame.min_x()
                        && x - fox::SIZE * 0.35 < window.frame.max_x()
                        && y < window.frame.max_y()
                        && y + fox::SIZE * 0.5 > window.frame.min_y()
                })
            };
            let can_burrow = fox_has_support(simulation, fox.x);
            if fox.advance(dt, snow_cursor, ground, simulation.width(), can_burrow, can_step) {
                self.fox = None;
            }
        }
        let bird_changed = self.bullfinch.is_some();
        if let Some(bird) = &mut self.bullfinch {
            let simulation = &self.simulation;
            let surfaces = &self.surfaces;
            let supported = bullfinch_has_support(simulation, surfaces, bird.support_x());
            let danger = self.plow.is_some() || self.truck.is_some();
            if bird.advance(
                dt,
                snow_cursor,
                simulation.width(),
                simulation.height(),
                supported,
                danger,
                |x| simulation.ground_surface(x),
                |x| bullfinch_has_support(simulation, surfaces, x),
            ) {
                self.bullfinch = None;
            }
        }
        let vehicle_moving = self.plow.is_some()
            || self.truck.is_some()
            || self.sleigh.is_some()
            || fox_changed
            || bird_changed
            || std::mem::take(&mut self.vehicle_dirty);
        if self.simulation.modes() != [Mode::Snow] {
            self.plow = None;
            self.truck = None;
            self.sleigh = None;
        } else if let Some(plow) = &mut self.plow {
            let (from, to, exited) = plow.advance(dt);
            // Keep clearing beneath the vehicle: the drift slumps back after each physics step.
            self.simulation.plow_ground(from - plow.direction * plow.sprite_width * 0.95, to);
            if exited {
                self.plow = None;
            }
        }
        if let Some(truck) = &mut self.truck {
            let (spray, exited) = truck.advance(dt);
            truck.follow_terrain(dt, |x| self.simulation.ground_surface(x));
            if spray {
                for x in truck.wheel_positions() {
                    self.simulation.spray_wheel(x, truck.direction);
                }
            }
            if exited {
                self.truck = None;
            }
        }
        if let Some(sleigh) = &mut self.sleigh {
            if sleigh.advance(dt) {
                self.sleigh = None;
            }
        }
        // Nothing to draw and nothing on its way: the frame shown last stays up, as it would, till the snow is stirred.
        let visible_sculptures = !self.sculpt.objects.stacks().is_empty()
            || !self.trees.objects.stacks().is_empty()
            || self.cone.is_some()
            || self.star_chain.is_some()
            || self.gifts.iter().any(|gift| !gift.stored);
        if !changed && !vehicle_moving && self.simulation.is_empty() && !visible_sculptures {
            self.fade(gpu, dt, true);
            self.last_step = 0;
            self.next_due = now + clock.frequency;
            return Ok(false);
        }
        // The lantern follows the cursor even over snow that lies still, on the display it lights.
        let lantern_moved = (sky.night > 0.0 || !self.trees.objects.stacks().is_empty())
            && (cursor - self.lantern_at).length() > 0.5
            && (self.lit_by(cursor) || self.lit_by(self.lantern_at));
        self.lantern_at = cursor;
        let fading = self.fade(gpu, dt, false);
        if self.hidden() {
            return Ok(false);
        }

        let fastest = if budget == PowerBudget::Full { self.interactive_rate } else { self.interactive_rate.min(60) };
        // 24 only where it divides the display's rate evenly; elsewhere it would judder.
        let calm = if budget == PowerBudget::Saver && self.interactive_rate % 24 == 0 { 24 } else { 30 };
        let rate = match self.simulation.pace() {
            Pace::Calm => calm,
            Pace::Lively => fastest.min(60),
            Pace::Interactive => fastest,
        };
        let rate = if self.max_rate {
            self.interactive_rate
        } else if lantern_moved || fading || vehicle_moving || interacting || (visible_sculptures && self.sculpt.objects.moving()) {
            rate.max(fastest.min(60))
        } else {
            rate
        };
        self.next_due = clock.schedule(now, rate);
        self.idle_frames = if changed || lantern_moved { 0 } else { self.idle_frames + 1 };
        // A still pile only needs a slow refresh for its glints.
        if !(changed
            || vehicle_moving
            || lantern_moved
            || fading
            || ((self.simulation.has_snow() || !self.simulation.pumpkins().is_empty()) && self.idle_frames % 2 == 0))
        {
            return Ok(false);
        }
        // Halloween night: cold moonlight, violet shadows and a candle's glow along the drifts' edges.
        let (light, shadow_tint) = moonlit(sky.daylight, self.shadow_tint, sky.night);
        let ambient = Ambient { light, shadow_tint, rim: [1.0, 0.56, 0.2, 0.3 * sky.night], lantern: (cursor, sky.night) };
        let mut sculptures = self.sculpt.sprites(&self.surfaces, &self.simulation);
        let snow_sculpture_count = sculptures.len();
        let mut tree_sculptures = self.trees.sprites(&self.surfaces, &self.simulation);
        if let Some(bird) = &self.bullfinch {
            let sprite = Sprite {
                position: V2::new(bird.position.x.round(), bird.position.y.round()),
                size: bullfinch::SIZE,
                alpha: 1.0,
                mood: [20.0, bird.tilt()],
                pose: [bird.frame(), if bird.facing_right() { 1.0 } else { 0.0 }, 0.0, 0.0],
                ..Sprite::default()
            };
            let covers = self
                .surfaces
                .iter()
                .map(|window| {
                    let r = window.frame;
                    [r.min_x(), r.min_y(), r.max_x(), r.max_y()]
                })
                .collect();
            sculptures.push(SculptureDraw { sprites: vec![sprite], covers, control: false });
        }
        if let Some(fox) = &self.fox {
            let ground = self.simulation.ground_surface(fox.x);
            let burrow = fox.burrow_frame();
            let sprite = Sprite {
                position: V2::new(fox.x.round(), (ground + fox::SIZE * 0.30 - burrow.map_or(0.0, |(_, below)| below)).round()),
                size: fox::SIZE,
                alpha: 1.0,
                mood: [if burrow.is_some() { 14.0 } else { 13.0 }, 0.0],
                pose: [burrow.map_or_else(|| fox.frame(), |(frame, _)| frame), if fox.facing_right() { 1.0 } else { 0.0 }, ground, 0.0],
                ..Sprite::default()
            };
            let covers: Vec<_> = self
                .surfaces
                .iter()
                .map(|window| {
                    let r = window.frame;
                    [r.min_x(), r.min_y(), r.max_x(), r.max_y()]
                })
                .collect();
            if let Some(below) = fox.bank_below() {
                let bank = Sprite {
                    position: V2::new(fox.x.round(), (ground + fox::SIZE * 0.30 - below).round()),
                    size: fox::SIZE,
                    alpha: 1.0,
                    mood: [14.0, 0.0],
                    pose: [0.0, if fox.facing_right() { 1.0 } else { 0.0 }, ground, 0.0],
                    ..Sprite::default()
                };
                sculptures.push(SculptureDraw { sprites: vec![bank], covers: covers.clone(), control: false });
            }
            sculptures.push(SculptureDraw { sprites: vec![sprite], covers, control: false });
        }
        if let Some(star) = &self.star_chain {
            let mut sprites = vec![Sprite { position: star.position, size: 48.0, alpha: 1.0, mood: [17.0, 0.0], ..Sprite::default() }];
            for i in 0..star.target {
                sprites.push(Sprite {
                    position: star.position + V2::new((i as f32 - (star.target - 1) as f32 * 0.5) * 6.0, -35.0),
                    size: 4.0,
                    alpha: if i < star.caught { 1.0 } else { 0.28 },
                    tint: [1.0, 0.76, 0.25],
                    tint_amount: 1.0,
                    mood: [2.0, 0.0],
                    ..Sprite::default()
                });
            }
            sculptures.push(SculptureDraw { sprites, covers: Vec::new(), control: true });
        }
        if let Some(cone) = &self.cone {
            let hovered = cone.hit(cursor)
                || self
                    .cone_input
                    .as_ref()
                    .is_some_and(|window| SCULPT_INPUT.with_borrow(|inputs| inputs.get(&(window.0.0 as isize)).is_some_and(|state| state.hovered)));
            let mut sprites = cone_sprites(cone.position, hovered && !cone.collected, cone.species);
            if cone.collected {
                let t = (cone.collection_time / 0.35).clamp(0.0, 1.0);
                let scale = (1.0 - t).powi(2).max(0.01);
                for sprite in &mut sprites {
                    sprite.size *= scale;
                    sprite.alpha *= 1.0 - t;
                }
                if !cone.planted && t >= 1.0 {
                    // A quiet glimmer marks a reward waiting for a safe snowy place.
                    sprites[0].size = 18.0;
                    sprites[0].alpha = 0.35;
                }
            }
            sculptures.push(SculptureDraw { sprites, covers: Vec::new(), control: true });
        }
        if let Some(drag) = &self.gift_drag
            && drag.moved
            && let Some((_, _, position)) = self.gift_target(drag.index)
        {
            let highlight = Sprite { position, size: 34.0, alpha: 0.55, mood: [8.0, 0.0], ..Sprite::default() };
            sculptures.push(SculptureDraw { sprites: vec![highlight], covers: Vec::new(), control: true });
        }
        // Draw the independent gift (and its reward) after the trees and other sculptures.
        for gift in &self.gifts {
            if !self.gift_visible(gift) || gift.size <= 0.0 {
                continue;
            }
            let sculptures = if gift.attachment.is_some() { &mut tree_sculptures } else { &mut sculptures };
            if let Some(age) = gift.broken {
                let particles = (0..6)
                    .map(|i| {
                        let dx = (i as f32 - 2.5) * gift.size * 0.9 * age;
                        let dy = (1.0 - (age / 0.6).min(1.0)).max(0.0) * age * gift.size * (1.5 + (i % 3) as f32 * 0.5);
                        Sprite {
                            position: V2::new(gift.x + dx, gift.bottom + dy + 3.0),
                            size: (gift.size * 0.14).max(3.0),
                            alpha: (1.0 - age / 0.6).clamp(0.0, 1.0),
                            tint: if i % 2 == 0 { [0.75, 0.08, 0.06] } else { [0.95, 0.65, 0.20] },
                            tint_amount: 1.0,
                            mood: [2.0, 0.0],
                            ..Sprite::default()
                        }
                    })
                    .collect();
                sculptures.push(SculptureDraw { sprites: particles, covers: Vec::new(), control: false });
                continue;
            }
            let covers: Vec<_> = if gift.support != 0 {
                self.surfaces
                    .iter()
                    .find(|window| window.id == gift.support)
                    .map(|window| window.cover.iter().map(|r| [r.min_x(), r.min_y(), r.max_x(), r.max_y()]).collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let opening = gift.opening.map(|elapsed| elapsed.clamp(0.0, 1.0));
            if gift.revealed || opening.is_some() {
                let progress = if gift.revealed { 1.0 } else { opening.unwrap_or(0.0) };
                let size = gift.size * 1.5;
                let lift = (1.0 - progress) * gift.size * 0.16;
                let center = V2::new(gift.x - gift.tilt.sin() * size * 0.30, gift.bottom + gift.tilt.cos() * size * 0.30 + lift);
                if gift.attachment.is_some() {
                    let length = gift.size * 0.32;
                    let ribbon = Sprite {
                        position: V2::new(gift.x, gift.bottom + gift.size * 0.96 + length * 0.5),
                        size: length,
                        alpha: 1.0,
                        tint: [0.96, 0.72, 0.30],
                        tint_amount: 1.0,
                        mood: [4.0, std::f32::consts::FRAC_PI_2],
                        pose: [0.12, 0.0, 0.0, 0.0],
                        ..Sprite::default()
                    };
                    sculptures.push(SculptureDraw { sprites: vec![ribbon], covers: covers.clone(), control: false });
                }
                let is_lantern = gift.kind == crate::gifts::GiftKind::Lantern;
                let sprite = Sprite {
                    position: center,
                    size,
                    alpha: (progress * 2.0).min(1.0),
                    mood: [if is_lantern { 18.0 } else { 16.0 }, gift.tilt],
                    pose: [0.0, if is_lantern && gift.enabled && gift.revealed { 1.0 } else { 0.0 }, 0.0, 0.0],
                    ..Sprite::default()
                };
                sculptures.push(SculptureDraw { sprites: vec![sprite], covers: covers.clone(), control: false });
            }
            if !gift.revealed {
                let (scale, bounce, alpha) = gift.entrance();
                let center = V2::new(
                    gift.x - gift.tilt.sin() * gift.size * scale * 0.5,
                    gift.bottom + gift.tilt.cos() * gift.size * scale * 0.5 + gift.size * bounce,
                );
                let sprite = Sprite {
                    position: center,
                    size: gift.size * scale * 1.35,
                    alpha,
                    mood: [15.0, gift.tilt],
                    pose: [opening.unwrap_or(0.0), if gift.color == crate::gifts::GiftColor::Green { 1.0 } else { 0.0 }, 0.0, 0.0],
                    ..Sprite::default()
                };
                sculptures.push(SculptureDraw { sprites: vec![sprite], covers, control: false });
            }
        }
        let together = self.trees_on_top || self.snow_behind_requested;
        let primary = order_sculptures(
            &sculptures,
            snow_sculpture_count,
            &tree_sculptures,
            together,
            self.trees_on_top && self.trees_before_snowmen,
            self.control.is_some(),
        );
        self.surface.extra_lights = if together {
            Vec::new()
        } else {
            tree_sculptures.iter().flat_map(|group| group.sprites.iter()).filter(|sprite| sprite.mood[0] == 18.0).copied().collect()
        };
        self.surface.draw(gpu, &self.simulation, ambient, time, self.plow.as_ref(), self.truck.as_ref(), self.sleigh.as_ref(), &primary, true)?;
        if let Some((_, surface)) = &mut self.control {
            let controls: Vec<_> = sculptures.iter().filter(|group| group.control).collect();
            surface.draw(gpu, &self.simulation, ambient, time, None, None, None, &controls, false)?;
        }
        if let Some((_, surface, _)) = &mut self.tree_overlay {
            let trees: Vec<_> = tree_sculptures.iter().collect();
            surface.draw(gpu, &self.simulation, ambient, time, None, None, None, &trees, false)?;
        }
        self.tree_dirty = false;
        Ok(true)
    }

    fn interaction_window(&self) -> HWND {
        self.control.as_ref().map_or(self.overlay.0, |(overlay, _)| overlay.0)
    }

    fn sync_cone_input(&mut self, tracker: &Tracker) {
        if self.covered || self.control_failed || self.cone_input_failed {
            self.cone_input = None;
            return;
        }
        let position = self
            .star_chain
            .as_ref()
            .map(|star| star.position)
            .or_else(|| self.cone.as_ref().filter(|cone| !cone.collected).map(|cone| cone.position));
        let Some(position) = position else {
            self.cone_input = None;
            return;
        };
        if self.layer.is_some()
            && tracker
                .covers(&self.monitor.rect, self.size)
                .iter()
                .any(|r| position.x >= r[0] && position.x <= r[2] && position.y >= r[1] && position.y <= r[3])
        {
            self.cone_input = None;
            return;
        }
        let rect = cone_input_rect(&self.monitor.rect, self.size, position);
        let newly_created = self.cone_input.is_none();
        if self.cone_input.is_none() {
            match create_cone_input(&rect, self.shown_in_screenshots) {
                Ok(window) => self.cone_input = Some(Overlay(window)),
                Err(error) => {
                    log!("cone input unavailable on {}: {error}", self.monitor.name);
                    self.cone_input_failed = true;
                    return;
                }
            }
        }
        if newly_created {
            self.raise_cone_input();
        }
        if let Some(window) = &self.cone_input {
            SCULPT_INPUT.with_borrow_mut(|inputs| {
                let state = inputs.entry(window.0.0 as isize).or_default();
                state.captures = true;
                state.is_cone = true;
                state.balls = vec![rect];
            });
            unsafe {
                let _ = SetWindowPos(
                    window.0,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOZORDER,
                );
            }
        }
    }

    fn free_tree_x(&self, desired: f32, support: isize, bottom: f32, height: f32, exclude: Option<u64>) -> Option<f32> {
        let (left, right) = if support == 0 {
            (0.0, self.simulation.width())
        } else {
            let window = self.surfaces.iter().find(|window| window.id == support)?;
            (window.frame.min_x(), window.frame.max_x())
        };
        let visible = |stack: &&Stack| !stack.support.is_some_and(|(id, _)| id != 0 && !self.surfaces.iter().any(|window| window.id == id));
        let mut occupants: Vec<Occupant> = self
            .sculpt
            .objects
            .stacks()
            .iter()
            .filter(visible)
            .map(|stack| {
                let radius = stack.radii.iter().copied().fold(0.0_f32, f32::max);
                Occupant {
                    x: stack.base.x,
                    bottom: stack.base.y,
                    half_width: radius * if stack.snowman() { 2.2 } else { 1.0 },
                    height: stack.height(),
                }
            })
            .collect();
        occupants.extend(self.trees.objects.stacks().iter().filter(visible).filter(|stack| Some(stack.appearance) != exclude).map(|stack| {
            let height = stack.height();
            Occupant { x: stack.base.x, bottom: stack.base.y, half_width: height * 0.375, height }
        }));
        // Reserve the tree's whole future silhouette against other windows as well.
        occupants.extend(self.surfaces.iter().filter(|window| window.id != support).map(|window| Occupant {
            x: (window.frame.min_x() + window.frame.max_x()) * 0.5,
            bottom: window.frame.min_y(),
            half_width: window.frame.width * 0.5,
            height: window.frame.height,
        }));
        tree_space::nearest(desired, left, right, bottom, height, height * 0.375, &occupants)
    }

    fn resolve_tree_placements(&mut self, before: &[Stack]) -> bool {
        let landed: Vec<Stack> = self
            .trees
            .objects
            .stacks()
            .iter()
            .filter(|stack| stack.support.is_some() && before.iter().any(|old| old.appearance == stack.appearance && old.support.is_none()))
            .cloned()
            .collect();
        let mut changed = false;
        for stack in landed {
            let (id, _) = stack.support.unwrap();
            let target = self
                .free_tree_x(stack.base.x, id, stack.base.y, stack.height(), Some(stack.appearance))
                .map(|x| {
                    let left = if id == 0 { 0.0 } else { self.surfaces.iter().find(|window| window.id == id).unwrap().frame.min_x() };
                    (V2::new(x, stack.base.y), (id, x - left))
                })
                .or_else(|| self.tree_origins.get(&stack.appearance).and_then(|&(base, support)| support.map(|support| (base, support))));
            if let Some((base, support)) = target {
                if (base - stack.base).length_squared() > 0.01 || Some(support) != stack.support {
                    changed |= self.trees.objects.relocate_resting(stack.appearance, base, support);
                }
            }
            self.tree_origins.remove(&stack.appearance);
        }
        self.tree_origins.retain(|id, _| self.trees.objects.stacks().iter().any(|stack| stack.appearance == *id));
        if changed {
            self.trees.persist_now();
        }
        changed
    }

    fn sculpt_capture(&self, capture: bool, active: bool) {
        let window = self.interaction_window();
        let id = window.0 as isize;
        let hit_rect = |(centre, radius): (V2, f32)| {
            let bounds = self.monitor.rect;
            let sx = (bounds.right - bounds.left) as f32 / self.size.0.max(1) as f32;
            let sy = (bounds.bottom - bounds.top) as f32 / self.size.1.max(1) as f32;
            RECT {
                left: bounds.left + ((centre.x - radius) * sx).floor() as i32,
                right: bounds.left + ((centre.x + radius) * sx).ceil() as i32,
                top: bounds.bottom - ((centre.y + radius) * sy).ceil() as i32,
                bottom: bounds.bottom - ((centre.y - radius) * sy).floor() as i32,
            }
        };
        let icon = self.sculpt.icon_hit_area().map(hit_rect);
        let mut areas = self.sculpt.hit_areas(&self.surfaces);
        areas.extend(self.trees.hit_areas(&self.surfaces));
        for gift in &self.gifts {
            if self.gift_visible(gift) && gift.size > 0.0 {
                areas.push((V2::new(gift.x, gift.bottom + gift.size * 0.5), gift.size * if gift.revealed { 0.8 } else { 0.62 }));
            }
        }
        if self.cone_input_failed
            && let Some(cone) = &self.cone
            && !cone.collected
        {
            areas.push((cone.position, HIT_RADIUS));
        }
        if self.cone_input_failed
            && let Some(star) = &self.star_chain
        {
            areas.push((star.position, crate::star_event::HIT_RADIUS));
        }
        let balls = areas.into_iter().map(hit_rect).collect();
        if active && SCULPT_ACTIVE.get() != id {
            let previous = SCULPT_ACTIVE.replace(id);
            if previous != 0 {
                SCULPT_INPUT.with_borrow_mut(|inputs| {
                    if let Some(state) = inputs.get_mut(&previous) {
                        state.events.push((true, None));
                        state.hotkey_registered = false;
                        state.hotkey_attempted = false;
                    }
                });
                unsafe {
                    let _ = UnregisterHotKey(Some(HWND(previous as _)), SCULPT_ESCAPE);
                }
            }
        } else if !active && SCULPT_ACTIVE.get() == id {
            SCULPT_ACTIVE.set(0);
        }
        let (changed, register, unregister) = SCULPT_INPUT.with_borrow_mut(|inputs| {
            let state = inputs.entry(id).or_default();
            let changed = state.captures != capture;
            state.captures = capture;
            state.active = active;
            state.icon = icon;
            state.balls = balls;
            let register = active && !state.hotkey_attempted;
            let unregister = !active && state.hotkey_registered;
            if register {
                state.hotkey_attempted = true;
            }
            if !active {
                state.hotkey_attempted = false;
                state.hotkey_registered = false;
            }
            (changed, register, unregister)
        });
        if unregister {
            unsafe {
                let _ = UnregisterHotKey(Some(window), SCULPT_ESCAPE);
            }
        }
        if register {
            let registered = unsafe { RegisterHotKey(Some(window), SCULPT_ESCAPE, MOD_NOREPEAT, VK_ESCAPE.0 as u32) }.is_ok();
            SCULPT_INPUT.with_borrow_mut(|inputs| {
                if let Some(state) = inputs.get_mut(&(window.0 as isize)) {
                    state.hotkey_registered = registered;
                }
            });
            if !registered {
                log!("Escape unavailable for snow sculpting; use the on-screen button to exit");
            }
        }
        if changed {
            unsafe {
                let style = GetWindowLongPtrW(window, GWL_EXSTYLE);
                let style = if capture { style & !(WS_EX_TRANSPARENT.0 as isize) } else { style | WS_EX_TRANSPARENT.0 as isize };
                SetWindowLongPtrW(window, GWL_EXSTYLE, style);
            }
        }
    }

    /// Whether a candle at this point, in overlay coordinates, lights anything on this display.
    fn lit_by(&self, at: V2) -> bool {
        let reach = LANTERN_REACH * 3.0;
        at.x > -reach && at.y > -reach && at.x < self.simulation.width() + reach && at.y < self.simulation.height() + reach
    }

    /// Hands the simulation the windows on this display when they changed.
    fn follow(&mut self, tracker: &Tracker, clock: &Clock, now: i64, snow_on_windows: bool) {
        let taskbar = if snow_on_windows { self.taskbar_surface() } else { None };
        let changed = !self.synced || tracker.generation != self.windows_seen || taskbar != self.taskbar;
        if !changed && !tracker.animating() {
            return;
        }
        let since = clock.seconds(now - self.windows_changed_at) as f32;
        // A window that moves outside a drag, as when it is maximized or snapped, jumps there in one go:
        // its snow is left behind instead of being carried along.
        let interval = if tracker.moving() || tracker.animating() || since < 0.1 { since.clamp(1.0 / 120.0, 0.25) } else { 1.0 / 60.0 };
        if changed {
            self.windows_seen = tracker.generation;
            self.taskbar = taskbar;
            if snow_on_windows {
                self.simulation.covers = tracker.covers(&self.monitor.rect, self.size);
            }
            let mut surfaces = tracker.surfaces(&self.monitor.rect, self.size);
            surfaces.extend(self.taskbar.clone());
            if !(self.synced && surfaces == self.surfaces) {
                self.synced = true;
                // Snow waits for a window gone to another desktop, or maximized or full screen and so without room for it.
                let elsewhere: HashSet<WindowId> = self
                    .surfaces
                    .iter()
                    .map(|surface| surface.id)
                    .filter(|&id| {
                        !surfaces.iter().any(|surface| surface.id == id) && (tracker.elsewhere(id) || tracker.fills(id, &self.monitor.rect))
                    })
                    .collect();
                if snow_on_windows {
                    self.simulation.update_windows(&surfaces, interval, &HashSet::new(), &elsewhere, false);
                }
                self.surfaces = surfaces;
                self.windows_changed_at = now;
            }
        }
        // A toast sliding in only sweeps the snow: the windows' ledges aren't rewritten every frame for it.
        if snow_on_windows && tracker.animating() {
            self.simulation.sweep_toasts(&tracker.toast_sweeps(&self.monitor.rect, self.size), interval);
            self.windows_changed_at = now;
        }
    }

    /// While it rains over a taskbar along the bottom of the display, the taskbar is a ledge, as the Dock is on the Mac:
    /// rain bursts on its top edge and runs down over its icons.
    fn taskbar_surface(&self) -> Option<WindowSurface> {
        let bounds = &self.monitor.bounds;
        if self.behind_taskbar || !self.simulation.raining() || self.monitor.rect.bottom != bounds.bottom {
            return None;
        }
        let mut rect = RECT::default();
        unsafe { GetWindowRect(taskbar(bounds)?, &mut rect) }.ok()?;
        // Only one along the bottom, and only while it shows: a hidden one leaves a sliver on the screen.
        let showing = rect.bottom.min(bounds.bottom) - rect.top.max(bounds.top);
        if rect.bottom < bounds.bottom || rect.right - rect.left < (bounds.right - bounds.left) / 2 || showing < 16 {
            return None;
        }
        Some(WindowSurface {
            id: TASKBAR,
            frame: local_frame(&rect, &self.monitor.rect, self.size)?,
            blockers: Vec::new(),
            cover: Vec::new(),
            corner_radius: 0.0,
            corner_exponent: 2.0,
        })
    }

    pub fn set_snow_on_windows(&mut self, enabled: bool) {
        if !enabled {
            self.simulation.update_windows(&[], 0.1, &HashSet::new(), &HashSet::new(), false);
            self.simulation.covers.clear();
        }
        self.synced = false;
        self.windows_seen = 0;
    }

    /// Fades the snow out while a full-screen app covers the display, still falling, then hides and stops it, as over
    /// a full-screen app on a Mac; once the app goes, the snow fades back in from where it stopped.
    pub fn set_covered(&mut self, gpu: &Gpu, covered: bool) {
        if covered == self.covered {
            return;
        }
        self.covered = covered;
        if covered {
            self.stop_sculpting();
        }
        if !covered && self.opacity <= 0.0 {
            let _ = self.surface.set_opacity(gpu, 0.0);
            if let Some((_, surface)) = &self.control {
                let _ = surface.set_opacity(gpu, 0.0);
            }
            unsafe {
                let _ = ShowWindow(self.overlay.0, SW_SHOWNOACTIVATE);
                if let Some((overlay, _)) = &self.control {
                    let _ = ShowWindow(overlay.0, SW_SHOWNOACTIVATE);
                }
            }
        }
        self.resume();
    }

    pub fn covered(&self) -> bool {
        self.covered
    }

    /// Faded all the way out under a full-screen app: nothing to draw until it goes.
    pub fn hidden(&self) -> bool {
        self.covered && self.opacity <= 0.0
    }

    /// Takes the fade a step toward shown, or gone under a full-screen app; true while it lasts.
    fn fade(&mut self, gpu: &Gpu, dt: f32, at_once: bool) -> bool {
        let target = if self.covered { 0.0 } else { 1.0 };
        if self.opacity == target {
            return false;
        }
        let step = if at_once { 1.0 } else { dt / if self.covered { FADE_OUT } else { FADE_IN } };
        self.opacity = if target > self.opacity { (self.opacity + step).min(1.0) } else { (self.opacity - step).max(0.0) };
        let _ = self.surface.set_opacity(gpu, self.opacity);
        if let Some((_, surface, _)) = &mut self.tree_overlay {
            let _ = surface.set_opacity(gpu, self.opacity);
        }
        if let Some((_, surface)) = &self.control {
            let _ = surface.set_opacity(gpu, self.opacity);
        }
        if self.hidden() {
            unsafe {
                let _ = ShowWindow(self.overlay.0, SW_HIDE);
                if let Some((overlay, _)) = &self.control {
                    let _ = ShowWindow(overlay.0, SW_HIDE);
                }
            }
        }
        true
    }

    /// Picks up the shadow colour again if the desktop picture changed.
    pub fn update_backdrop(&mut self, wallpapers: &Wallpapers) {
        let backdrop = wallpapers.backdrop(&self.monitor.bounds);
        if backdrop == self.backdrop {
            return;
        }
        self.shadow_tint = backdrop.as_ref().and_then(|backdrop| wallpapers.tint(backdrop)).unwrap_or([0.0; 4]);
        log!("backdrop of {}: {backdrop:?}, shadow {:?}", self.monitor.name, self.shadow_tint);
        self.backdrop = backdrop;
    }

    pub fn wake(&mut self, now: i64) {
        self.next_due = self.next_due.min(now);
    }

    pub fn stop_plow(&mut self, now: i64) {
        if self.plow.take().is_some() {
            self.vehicle_dirty = true;
            self.wake(now);
        }
    }

    pub fn stop_truck(&mut self, now: i64) {
        if self.truck.take().is_some() {
            self.vehicle_dirty = true;
            self.wake(now);
        }
    }

    pub fn stop_sleigh(&mut self, now: i64) {
        if self.sleigh.take().is_some() {
            self.vehicle_dirty = true;
            self.wake(now);
        }
    }

    pub fn resume(&mut self) {
        self.last_step = 0;
        self.next_due = 0;
    }

    pub fn stop_sculpting(&mut self) {
        self.sculpt.stop();
        self.trees.stop();
        self.cone_input = None;
        self.sculpt_capture(false, false);
        unsafe {
            let _ = ReleaseCapture();
        }
    }

    pub fn has_cone(&self) -> bool {
        self.cone.is_some()
    }

    pub fn has_legendary_tree(&self, variant: u8) -> bool {
        self.trees.has_legendary_tree(variant) || self.cone.as_ref().is_some_and(|cone| cone.legendary == Some(variant))
    }

    pub fn has_star_chain(&self) -> bool {
        self.star_chain.is_some()
    }

    pub fn can_start_star_chain(&self) -> bool {
        self.star_chain.is_none() && self.cone.is_none() && self.trees.can_plant_legendary()
    }

    pub fn start_star_chain(&mut self, random: u64, owned: usize, reward: u8, now: i64) {
        if self.can_start_star_chain() {
            self.star_chain = Some(crate::star_event::StarChain::new(self.simulation.width(), self.simulation.height(), random, owned, reward));
            self.wake(now);
        }
    }

    fn catch_star(&mut self) {
        let Some(star) = &mut self.star_chain else { return };
        let position = star.position;
        if star.catch(self.simulation.width(), self.simulation.height()) {
            let mut cone = Cone::new(self.simulation.width(), self.simulation.height(), star.reward_seed());
            cone.position = position;
            cone.species = crate::cone::Species::Spruce;
            cone.legendary = Some(star.reward);
            cone.collected = true;
            self.cone = Some(cone);
            self.star_chain = None;
        }
    }

    fn gift_visible(&self, gift: &crate::gifts::Gift) -> bool {
        !gift.stored
            && gift.pending_window.is_none()
            && gift.attachment.is_none_or(|(id, _)| self.trees.objects.stacks().iter().any(|tree| tree.appearance == id))
            && (gift.support == 0 || gift.support == -1 || self.surfaces.iter().any(|window| window.id == gift.support))
    }

    pub fn collection_dragging(&self) -> bool {
        self.trees.objects.holding() || self.gift_drag.as_ref().is_some_and(|drag| drag.moved)
    }

    fn store_dragged_tree(&mut self) -> std::result::Result<usize, crate::snow::CollectionError> {
        use crate::snow::{CollectionError as E, CollectionObject};
        let Some(id) = self.trees.objects.held_stack().map(|tree| tree.appearance) else { return Err(E::Unavailable) };
        let origin = self.tree_origins.remove(&id).and_then(|(base, support)| support.map(|support| (base, support)));
        let Some((base, support)) = origin else {
            self.trees.stop();
            return Err(E::Unavailable);
        };
        if !self.trees.restore_held(base, support) {
            self.trees.stop();
            return Err(E::Unavailable);
        }
        let (species, variant) = self.trees.tree_style(id);
        self.move_collection_item(CollectionObject::Tree(id, species, variant), true)
    }

    fn store_dragged_gift(&mut self) -> std::result::Result<usize, crate::snow::CollectionError> {
        use crate::snow::CollectionError as E;
        let Some(drag) = self.gift_drag.take() else { return Err(E::Unavailable) };
        self.gift_pressed = false;
        let Some(gift) = self.gifts.get_mut(drag.index) else { return Err(E::Unavailable) };
        *gift = drag.original.clone();
        if !self.set_gifts_stored(&[drag.index], true) {
            return Err(E::SaveFailed);
        }
        self.collection_undo = Some(CollectionUndo::Gift(drag.original));
        Ok(0)
    }

    pub fn collection_entries(&self) -> Vec<crate::snow::CollectionEntry> {
        use crate::snow::{CollectionEntry, CollectionObject};
        let monitor = &self.monitor.name;
        let mut entries: Vec<_> = self
            .trees
            .collection_trees()
            .into_iter()
            .map(|(id, species, variant, stored, height_mm)| CollectionEntry {
                monitor: monitor.clone(),
                object: CollectionObject::Tree(id, species, variant),
                stored,
                tree_height_mm: Some(height_mm),
            })
            .collect();
        entries.extend(self.gifts.iter().filter(|gift| gift.revealed && gift.broken.is_none() && gift.size > 0.0).map(|gift| CollectionEntry {
            monitor: monitor.clone(),
            object: CollectionObject::Gift(gift.kind),
            stored: gift.stored,
            tree_height_mm: None,
        }));
        entries
    }

    pub fn move_collection_item(
        &mut self,
        object: crate::snow::CollectionObject,
        stored: bool,
    ) -> std::result::Result<usize, crate::snow::CollectionError> {
        use crate::snow::{CollectionError as E, CollectionObject};
        if self.gift_drag.is_some() || self.gift_pressed || self.trees.objects.holding() {
            return Err(E::Busy);
        }
        match object {
            CollectionObject::Gift(kind) => {
                let Some(index) =
                    self.gifts.iter().position(|gift| gift.kind == kind && gift.revealed && gift.broken.is_none() && gift.stored != stored)
                else {
                    return Err(E::Unavailable);
                };
                let before = self.gifts[index].clone();
                if !self.set_gifts_stored(&[index], stored) {
                    return Err(E::SaveFailed);
                }
                self.collection_undo = if stored { Some(CollectionUndo::Gift(before)) } else { None };
                Ok(0)
            }
            CollectionObject::Tree(id, _, _) => {
                if stored {
                    let Some(tree) = self.trees.objects.stacks().iter().find(|tree| tree.appearance == id && tree.support.is_some()) else {
                        return Err(E::Unavailable);
                    };
                    let (base, support) = (tree.base, tree.support.unwrap());
                    let attached: Vec<_> = self
                        .gifts
                        .iter()
                        .enumerate()
                        .filter(|(_, gift)| !gift.stored && gift.attachment.is_some_and(|(tree, _)| tree == id))
                        .map(|(index, _)| index)
                        .collect();
                    let gifts: Vec<_> = attached.iter().map(|&index| self.gifts[index].clone()).collect();
                    if !attached.is_empty() && !self.set_gifts_stored(&attached, true) {
                        return Err(E::SaveFailed);
                    }
                    if !self.trees.set_tree_stored(id, true, None) {
                        for (&index, gift) in attached.iter().zip(&gifts) {
                            self.gifts[index] = gift.clone();
                        }
                        let _ = self.save_gifts();
                        return Err(E::SaveFailed);
                    }
                    let count = gifts.len();
                    self.collection_undo = Some(CollectionUndo::Tree { id, base, support, gifts });
                    Ok(count)
                } else {
                    let Some((desired, height)) = self.trees.stored_tree_size(id) else { return Err(E::Unavailable) };
                    let Some(x) = self.free_tree_x(desired, 0, self.simulation.ground_surface(desired), height, None) else { return Err(E::NoRoom) };
                    let placement = Some((V2::new(x, self.simulation.ground_surface(x)), (0, x)));
                    if self.trees.set_tree_stored(id, false, placement) {
                        self.collection_undo = None;
                        Ok(0)
                    } else {
                        Err(E::SaveFailed)
                    }
                }
            }
        }
    }

    pub fn undo_collection_move(&mut self) -> std::result::Result<(), crate::snow::CollectionError> {
        use crate::snow::CollectionError as E;
        if self.gift_drag.is_some() || self.gift_pressed || self.trees.objects.holding() {
            return Err(E::Busy);
        }
        let Some(undo) = self.collection_undo.take() else { return Err(E::Unavailable) };
        let gifts = match &undo {
            CollectionUndo::Tree { gifts, .. } => gifts.as_slice(),
            CollectionUndo::Gift(gift) => std::slice::from_ref(gift),
        };
        let indices: Option<Vec<_>> =
            gifts.iter().map(|old| self.gifts.iter().position(|gift| gift.kind == old.kind && gift.stored && gift.broken.is_none())).collect();
        let Some(indices) = indices else {
            self.collection_undo = Some(undo);
            return Err(E::Unavailable);
        };
        if gifts.iter().any(|gift| {
            gift.attachment.is_some_and(|(id, _)| {
                !matches!(&undo, CollectionUndo::Tree { id: tree, .. } if *tree == id)
                    && !self.trees.objects.stacks().iter().any(|tree| tree.appearance == id)
            }) || (gift.attachment.is_none()
                && gift.pending_window.is_none()
                && gift.support > 0
                && !self.surfaces.iter().any(|surface| surface.id == gift.support))
        }) {
            self.collection_undo = Some(undo);
            return Err(E::Unavailable);
        }
        if gifts.iter().any(|old| {
            old.attachment
                .is_some_and(|(tree, _)| self.gifts.iter().any(|gift| !gift.stored && gift.attachment.is_some_and(|(other, _)| other == tree)))
        }) {
            self.collection_undo = Some(undo);
            return Err(E::NoRoom);
        }
        if let CollectionUndo::Tree { id, base, support, .. } = &undo {
            let base = if support.0 == 0 {
                V2::new(base.x, self.simulation.ground_surface(base.x))
            } else if let Some(surface) = self.surfaces.iter().find(|surface| surface.id == support.0) {
                let x = surface.frame.min_x() + support.1;
                V2::new(x, self.simulation.snow_surface_on(surface.id, x).unwrap_or(surface.frame.max_y()))
            } else {
                self.collection_undo = Some(undo);
                return Err(E::Unavailable);
            };
            let Some((_, height)) = self.trees.stored_tree_size(*id) else {
                self.collection_undo = Some(undo);
                return Err(E::Unavailable);
            };
            if self.free_tree_x(base.x, support.0, base.y, height, None).is_none_or(|x| (x - base.x).abs() > 1.0) {
                self.collection_undo = Some(undo);
                return Err(E::NoRoom);
            }
            if !self.trees.set_tree_stored(*id, false, Some((base, *support))) {
                self.collection_undo = Some(undo);
                return Err(E::SaveFailed);
            }
        }
        let before: Vec<_> = indices.iter().map(|&index| self.gifts[index].clone()).collect();
        for (&index, gift) in indices.iter().zip(gifts) {
            self.gifts[index] = gift.clone();
        }
        if !indices.is_empty() && !self.save_gifts() {
            for (&index, gift) in indices.iter().zip(before) {
                self.gifts[index] = gift;
            }
            if let CollectionUndo::Tree { id, .. } = &undo {
                let _ = self.trees.set_tree_stored(*id, true, None);
            }
            self.collection_undo = Some(undo);
            return Err(E::SaveFailed);
        }
        Ok(())
    }

    /// Place a stored object on the first supporting ledge below the drop, or on the ground.
    pub fn place_collection_item(&mut self, object: crate::snow::CollectionObject, at: V2) -> std::result::Result<(), crate::snow::CollectionError> {
        use crate::snow::{CollectionError as E, CollectionObject};
        if self.gift_drag.is_some() || self.gift_pressed || self.trees.objects.holding() {
            return Err(E::Busy);
        }
        if at.x < 0.0 || at.x >= self.simulation.width() || at.y < 0.0 || at.y > self.simulation.height() {
            return Err(E::NoRoom);
        }
        let ground = self.simulation.ground_surface(at.x);
        let surfaces = self
            .surfaces
            .iter()
            .filter(|window| at.x >= window.frame.min_x() && at.x <= window.frame.max_x())
            .map(|window| (window.id, window.frame.min_x(), self.simulation.snow_surface_on(window.id, at.x).unwrap_or(window.frame.max_y())));
        let surface = collection_drop_surface(at.y, ground, surfaces);
        match object {
            CollectionObject::Tree(id, _, _) => {
                let Some((_, height)) = self.trees.stored_tree_size(id) else { return Err(E::Unavailable) };
                let Some(x) = self.free_tree_x(at.x, surface.0, surface.2, height, None) else { return Err(E::NoRoom) };
                // Do not silently move the tree a long way from where it was dropped.
                if (x - at.x).abs() > 15.0 {
                    return Err(E::NoRoom);
                }
                if self.trees.set_tree_stored(id, false, Some((V2::new(x, surface.2), (surface.0, x - surface.1)))) {
                    self.collection_undo = None;
                    Ok(())
                } else {
                    Err(E::SaveFailed)
                }
            }
            CollectionObject::Gift(kind) => {
                let Some(index) = self.gifts.iter().position(|gift| gift.kind == kind && gift.stored && gift.revealed) else {
                    return Err(E::Unavailable);
                };
                let gift = &mut self.gifts[index];
                let old = (gift.x, gift.bottom, gift.support, gift.offset, gift.attachment, gift.pending_window.clone(), gift.tilt);
                gift.x = at.x;
                gift.bottom = at.y - gift.size * 0.45;
                let target = self.gift_target(index);
                let gift = &mut self.gifts[index];
                gift.stored = false;
                gift.pending_window = None;
                gift.tilt = 0.0;
                if let Some((tree, slot, centre)) = target {
                    gift.attachment = Some((tree, slot));
                    gift.x = centre.x;
                    gift.bottom = centre.y - gift.size * 1.5 * 0.30;
                } else {
                    gift.attachment = None;
                    gift.bottom = surface.2;
                    gift.support = surface.0;
                    gift.offset = gift.x - surface.1;
                }
                if self.save_gifts() {
                    self.collection_undo = None;
                    Ok(())
                } else {
                    let gift = &mut self.gifts[index];
                    gift.stored = true;
                    (gift.x, gift.bottom, gift.support, gift.offset, gift.attachment, gift.pending_window, gift.tilt) = old;
                    Err(E::SaveFailed)
                }
            }
        }
    }

    /// Write all affected decorations together before removing their tree.
    fn set_gifts_stored(&mut self, indices: &[usize], stored: bool) -> bool {
        if !self.gifts_writable {
            return false;
        }
        let before: Vec<_> = indices
            .iter()
            .map(|&index| {
                let gift = &mut self.gifts[index];
                let old = (gift.stored, gift.attachment, gift.pending_window.clone(), gift.support, gift.offset, gift.tilt);
                gift.stored = stored;
                gift.attachment = None;
                gift.pending_window = None;
                gift.support = 0;
                gift.offset = gift.x;
                gift.tilt = 0.0;
                old
            })
            .collect();
        if !self.save_gifts() {
            for (&index, (stored, attachment, pending_window, support, offset, tilt)) in indices.iter().zip(before) {
                let gift = &mut self.gifts[index];
                gift.stored = stored;
                gift.attachment = attachment;
                gift.pending_window = pending_window;
                gift.support = support;
                gift.offset = offset;
                gift.tilt = tilt;
            }
            return false;
        }
        true
    }

    fn move_gift_drag(&mut self, pointer: V2) {
        let Some(drag) = &mut self.gift_drag else { return };
        if !drag.moved && (pointer - drag.start).length_squared() < 25.0 {
            return;
        }
        if let Some(gift) = self.gifts.get_mut(drag.index) {
            if !drag.moved {
                gift.begin_fall();
            }
            drag.moved = true;
            let base = pointer + drag.offset;
            gift.x = base.x;
            gift.bottom = base.y;
            gift.tilt = 0.0;
        }
    }

    fn gift_target(&self, index: usize) -> Option<(u64, u8, V2)> {
        let gift = self.gifts.get(index)?;
        let centre = V2::new(gift.x, gift.bottom + gift.size * 0.45);
        self.trees
            .objects
            .stacks()
            .iter()
            .filter(|tree| crate::sculpt::adult_tree(tree.appearance))
            .filter(|tree| tree.support.is_none_or(|(id, _)| id == 0 || self.surfaces.iter().any(|window| window.id == id)))
            .filter(|tree| {
                !self.gifts.iter().enumerate().any(|(other, gift)| other != index && gift.attachment.is_some_and(|(id, _)| id == tree.appearance))
            })
            .filter_map(|tree| {
                let (species, variant) = self.trees.tree_style(tree.appearance);
                let slots = crate::ornaments::tree_slots(species, variant);
                let height = tree.height_from_radii();
                let slot = crate::ornaments::nearest_slot(
                    &slots,
                    &[],
                    tree.base,
                    height,
                    centre,
                    V2::new(gift.size * 1.5, gift.size * 0.85),
                    (height * 0.20).max(28.0),
                )?;
                let position = slots.iter().find(|entry| entry.id == slot)?.world_position(tree.base, height);
                Some((tree.appearance, slot, position))
            })
            .min_by(|a, b| (a.2 - centre).length_squared().total_cmp(&(b.2 - centre).length_squared()))
    }

    fn release_gift_drag(&mut self, attach: bool) {
        let Some(drag) = self.gift_drag.take() else { return };
        if !drag.moved {
            if attach
                && let Some(gift) = self.gifts.get_mut(drag.index)
                && gift.kind == crate::gifts::GiftKind::Lantern
                && gift.revealed
            {
                gift.enabled = !gift.enabled;
                self.gifts_save_delay = 0.0;
            }
            return;
        }
        let target = if attach { self.gift_target(drag.index) } else { None };
        if let Some(gift) = self.gifts.get_mut(drag.index) {
            if let Some((tree, slot, centre)) = target {
                gift.attachment = Some((tree, slot));
                gift.x = centre.x;
                gift.bottom = centre.y - gift.size * 1.5 * 0.30;
                gift.tilt = 0.0;
            } else {
                gift.begin_fall();
            }
        }
        self.gifts_save_delay = 0.0;
    }

    fn save_gifts(&mut self) -> bool {
        if !self.gifts_writable {
            return false;
        }
        let mut saved = Vec::new();
        for gift in self.gifts.iter().filter(|gift| gift.revealed && gift.size > 0.0 && gift.broken.is_none()) {
            let support = if gift.stored {
                crate::gifts::SavedSupport::Stored
            } else if let Some((id, slot)) = gift.attachment {
                crate::gifts::SavedSupport::Tree(id, slot)
            } else if let Some((key, fraction)) = &gift.pending_window {
                crate::gifts::SavedSupport::Window(key.clone(), *fraction)
            } else if gift.support == -1 {
                crate::gifts::SavedSupport::Falling
            } else if gift.support == 0 {
                crate::gifts::SavedSupport::Ground
            } else {
                let Some(window) = self.surfaces.iter().find(|window| window.id == gift.support) else { return false };
                let Some(key) = crate::sculpt::window_key(window.id) else { return false };
                if window.frame.width <= 0.0 {
                    return false;
                }
                crate::gifts::SavedSupport::Window(key, (gift.offset / window.frame.width).clamp(0.0, 1.0))
            };
            saved.push(crate::gifts::SavedGift {
                kind: gift.kind,
                enabled: gift.enabled,
                x: (gift.x / self.size.0.max(1) as f32).clamp(-1.0, 2.0),
                y: (gift.bottom / self.size.1.max(1) as f32).clamp(-1.0, 2.0),
                size: gift.size,
                support,
            });
        }
        let value = crate::gifts::encode_saved(&saved);
        if value != self.gifts_saved {
            if crate::prefs::set_sculptures(&format!("boomboxes:{}", self.monitor.name), &value) {
                self.gifts_saved = value;
            } else {
                log!("could not save boomboxes on {}", self.monitor.name);
                return false;
            }
        }
        true
    }

    pub fn gift_count(&self) -> usize {
        self.gifts.iter().filter(|gift| gift.size > 0.0 && gift.broken.is_none()).count()
    }

    pub fn tree_population(&self) -> [usize; 3] {
        self.trees.tree_population()
    }

    /// Find supported adult trees with a visible, unobstructed spot beside the trunk.
    fn gift_places(&self) -> Vec<(u64, f32)> {
        let mut places = Vec::new();
        for tree in self.trees.objects.stacks() {
            let Some((support, _)) = tree.support else { continue };
            if !crate::sculpt::adult_tree(tree.appearance) {
                continue;
            }
            let height = tree.height_from_radii();
            let size = (height * 0.26).clamp(26.0, 45.0);
            for side in [-1.0, 1.0] {
                let x = tree.base.x + side * height * 0.19;
                let bottom = tree.base.y;
                if x - size * 0.5 < 0.0 || x + size * 0.5 > self.simulation.width() || bottom + size > self.simulation.height() {
                    continue;
                }
                if support != 0
                    && !self
                        .surfaces
                        .iter()
                        .any(|window| window.id == support && x - size * 0.5 >= window.frame.min_x() && x + size * 0.5 <= window.frame.max_x())
                {
                    continue;
                }
                if self.surfaces.iter().any(|window| {
                    window.id != support
                        && x + size * 0.5 > window.frame.min_x()
                        && x - size * 0.5 < window.frame.max_x()
                        && bottom < window.frame.max_y()
                        && bottom + size > window.frame.min_y()
                }) {
                    continue;
                }
                if self.gifts.iter().any(|gift| {
                    !gift.stored
                        && gift.support == support
                        && (gift.x - x).abs() < (gift.size * 0.8 + size * 0.5)
                        && (gift.bottom - bottom).abs() < size
                }) {
                    continue;
                }
                places.push((tree.appearance, side));
            }
        }
        places
    }

    pub fn can_spawn_gift(&self) -> bool {
        self.gifts_writable && self.gifts.len() < 64 && crate::gifts::available_kind(&self.gifts, 0).is_some() && !self.gift_places().is_empty()
    }

    pub fn spawn_gift(&mut self, random: u64, color_random: u64, now: i64) {
        if !self.gifts_writable || self.gifts.len() >= 64 {
            return;
        }
        let Some(kind) = crate::gifts::available_kind(&self.gifts, random.rotate_right(32)) else { return };
        let places = self.gift_places();
        if !places.is_empty() {
            let (id, side) = places[random as usize % places.len()];
            if let Some(tree) = self.trees.objects.stacks().iter().find(|tree| tree.appearance == id)
                && let Some((support, _)) = tree.support
            {
                let height = tree.height_from_radii();
                let size = (height * 0.26).clamp(26.0, 45.0);
                let x = tree.base.x + side * height * 0.19;
                let left = if support == 0 {
                    0.0
                } else {
                    self.surfaces.iter().find(|window| window.id == support).map_or(0.0, |window| window.frame.min_x())
                };
                let color = crate::gifts::GiftColor::from_random(color_random);
                self.gifts.push(crate::gifts::Gift::with_kind(kind, color, x, tree.base.y, size, support, x - left));
                self.wake(now);
            }
        }
    }

    pub fn has_fox(&self) -> bool {
        self.fox.is_some()
    }

    pub fn has_bullfinch(&self) -> bool {
        self.bullfinch.is_some()
    }

    pub fn spawn_bullfinch(&mut self, random: u64, now: i64) -> bool {
        if self.bullfinch.is_some() || self.simulation.width() < bullfinch::SIZE {
            return false;
        }
        let width = self.simulation.width();
        for attempt in 0_u64..12 {
            let fraction = ((random.wrapping_add(attempt.wrapping_mul(0x9e3779b97f4a7c15)) >> 32) as u32) as f32 / u32::MAX as f32;
            let x = bullfinch::SIZE * 0.5 + fraction * (width - bullfinch::SIZE);
            if bullfinch_has_support(&self.simulation, &self.surfaces, x) {
                self.bullfinch = Some(Bullfinch::new(x, self.simulation.ground_surface(x), width, self.simulation.height(), random & 1 == 0));
                self.wake(now);
                return true;
            }
        }
        false
    }

    /// Pick a snowy, unobstructed patch on the ground; try again later if none is available.
    pub fn spawn_fox(&mut self, random: u64, now: i64) -> bool {
        if self.fox.is_some() || self.simulation.width() < fox::SIZE {
            return false;
        }
        let width = self.simulation.width();
        for attempt in 0_u64..12 {
            let fraction = ((random.wrapping_add(attempt.wrapping_mul(0x9e3779b97f4a7c15)) >> 32) as u32) as f32 / u32::MAX as f32;
            let x = fox::SIZE * 0.5 + fraction * (width - fox::SIZE);
            let ground = self.simulation.ground_surface(x);
            let clear = !self.surfaces.iter().any(|window| {
                x + fox::SIZE * 0.35 > window.frame.min_x()
                    && x - fox::SIZE * 0.35 < window.frame.max_x()
                    && ground < window.frame.max_y()
                    && ground + fox::SIZE * 0.5 > window.frame.min_y()
            });
            if clear && fox_has_support(&self.simulation, x) {
                self.fox = Some(Fox::new(x, random & 1 == 0));
                self.wake(now);
                return true;
            }
        }
        false
    }

    pub fn spawn_cone(&mut self, random: u64, now: i64) {
        if self.cone.is_none() {
            self.cone = Some(Cone::new(self.simulation.width(), self.simulation.height(), random));
            self.wake(now);
        }
    }

    pub fn show_in_screenshots(&mut self, shown: bool) {
        self.shown_in_screenshots = shown;
        set_capture(self.overlay.0, shown);
        if let Some((overlay, _, _)) = &self.tree_overlay {
            set_capture(overlay.0, shown);
        }
        if let Some(window) = &self.cone_input {
            set_capture(window.0, shown);
        }
        if let Some((overlay, _)) = &self.control {
            set_capture(overlay.0, shown);
        }
    }

    /// Topmost windows opened later stack above; this puts the snow back on top of them, though still under open menus,
    /// tooltips and the shell's flyouts, as menus stay in front of the snow on a Mac.
    /// Behind windows, the snow is kept just under the desktop icons instead, and above live wallpapers
    /// like Lively's, which go into the same layer and land on top when they start later.
    /// Behind the taskbar, it is kept just under the display's taskbar, whose icons stay in front of the drift.
    pub fn raise(&self) {
        if let Some((overlay, _, layer)) = &self.tree_overlay {
            let above = layer.map_or_else(|| lowest_open_popup().unwrap_or(HWND_TOPMOST), |layer| layer.icons.unwrap_or(HWND_TOP));
            if layer.is_none_or(|layer| behind_needs_raise(overlay.0, layer.icons)) {
                unsafe {
                    let _ = SetWindowPos(overlay.0, Some(above), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
                }
            }
        }
        if let Some((overlay, _)) = &self.control {
            let above = lowest_open_popup().unwrap_or(HWND_TOPMOST);
            unsafe {
                let _ = SetWindowPos(overlay.0, Some(above), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
            }
        }
        // A taskbar out of the topmost band, as under a full-screen app, would take the snow down with it.
        let topmost = |window: &HWND| unsafe { GetWindowLongPtrW(*window, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 != 0;
        let taskbar = if self.behind_taskbar { taskbar(&self.monitor.bounds).filter(topmost) } else { None };
        let above = match (self.layer, taskbar) {
            (Some(layer), _) => layer.icons.unwrap_or(HWND_TOP),
            (None, Some(taskbar)) => taskbar,
            (None, None) => lowest_open_popup().unwrap_or(HWND_TOPMOST),
        };
        // Several monitors share Progman: only one overlay can sit immediately under the icons. Raising every
        // overlay to that slot each second makes them shove each other and the snow blinks on one display or
        // another. Skip when we already hang under the icons with only our own overlays in between.
        if let Some(layer) = self.layer
            && !behind_needs_raise(self.overlay.0, layer.icons)
        {
            self.raise_cone_input();
            return;
        }
        unsafe {
            let _ = SetWindowPos(self.overlay.0, Some(above), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
        }
        self.raise_cone_input();
    }

    fn raise_cone_input(&self) {
        if let Some(window) = &self.cone_input {
            let above = lowest_open_popup().unwrap_or(HWND_TOPMOST);
            unsafe {
                let _ = SetWindowPos(window.0, Some(above), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
            }
        }
    }

    /// Whether this display's taskbar, hiding itself where the snow lies, has come over the overlay, as it does when clicked.
    pub fn under_taskbar(&self) -> bool {
        self.layer.is_none()
            && !self.behind_taskbar
            && self.monitor.rect == self.monitor.bounds
            && taskbar_above(self.overlay.0, &self.monitor.bounds)
    }

    /// An overlay inside Explorer's wallpaper layer goes away with it when Explorer restarts.
    pub fn alive(&self) -> bool {
        (unsafe { IsWindow(Some(self.overlay.0)).as_bool() })
            && self.control.as_ref().is_none_or(|(overlay, _)| unsafe { IsWindow(Some(overlay.0)).as_bool() })
    }

    pub fn behind_windows(&self) -> bool {
        self.layer.is_some()
    }

    /// The pointer in overlay coordinates.
    fn cursor(&self) -> V2 {
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_err() {
            return V2::new(-10_000.0, -10_000.0);
        }
        self.local(point.x as f32, point.y as f32)
    }

    /// A point in physical screen pixels in overlay coordinates: DIPs up from the bottom left of this display's work area.
    pub fn local(&self, x: f32, y: f32) -> V2 {
        let rect = &self.monitor.rect;
        let (width, height) = physical_size(&self.monitor);
        V2::new((x - rect.left as f32) * self.size.0 as f32 / width as f32, (rect.bottom as f32 - y) * self.size.1 as f32 / height as f32)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        let bounds = &self.monitor.bounds;
        x >= bounds.left as f32 && x < bounds.right as f32 && y >= bounds.top as f32 && y < bounds.bottom as f32
    }
}

/// Prefer the highest support below the pointer, allowing a small tolerance for a ledge's edge.
fn collection_drop_surface(pointer_y: f32, ground: f32, candidates: impl Iterator<Item = (isize, f32, f32)>) -> (isize, f32, f32) {
    candidates
        .filter(|candidate| candidate.2 <= pointer_y + 12.0 && candidate.2 > ground)
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .unwrap_or((0, 0.0, ground))
}

#[cfg(test)]
mod collection_drop_tests {
    use super::collection_drop_surface;

    #[test]
    fn chooses_the_highest_ledge_below_the_drop_or_ground() {
        let ledges = [(1, 20.0, 50.0), (2, 10.0, 110.0), (3, 30.0, 200.0)];
        assert_eq!(collection_drop_surface(120.0, 5.0, ledges.into_iter()), (2, 10.0, 110.0));
        assert_eq!(collection_drop_surface(100.0, 5.0, ledges.into_iter()), (2, 10.0, 110.0));
        assert_eq!(collection_drop_surface(70.0, 5.0, ledges.into_iter()), (1, 20.0, 50.0));
        assert_eq!(collection_drop_surface(10.0, 5.0, ledges.into_iter()), (0, 0.0, 5.0));
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop, Direct3D and an isolated temporary registry key"]
    fn undo_restores_tree_and_attached_gift() {
        use super::*;
        use windows::Win32::Foundation::ERROR_SUCCESS;
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        use windows::Win32::System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_ALL_ACCESS, REG_OPTION_NON_VOLATILE, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOverridePredefKey,
        };
        use windows::core::{PCWSTR, w};

        const CHILD: &str = "SNOWHUSH_INVENTORY_UNDO_TEST_ROOT";
        if let Ok(path) = std::env::var(CHILD) {
            let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
            let mut root = HKEY::default();
            unsafe {
                assert_eq!(
                    RegCreateKeyExW(
                        HKEY_CURRENT_USER,
                        PCWSTR(path.as_ptr()),
                        None,
                        PCWSTR::null(),
                        REG_OPTION_NON_VOLATILE,
                        KEY_ALL_ACCESS,
                        None,
                        &mut root,
                        None
                    ),
                    ERROR_SUCCESS
                );
                assert_eq!(RegOverridePredefKey(HKEY_CURRENT_USER, Some(root)), ERROR_SUCCESS);
                let mut key = HKEY::default();
                assert_eq!(
                    RegCreateKeyExW(
                        HKEY_CURRENT_USER,
                        w!("Software\\VelvetFrost"),
                        None,
                        PCWSTR::null(),
                        REG_OPTION_NON_VOLATILE,
                        KEY_ALL_ACCESS,
                        None,
                        &mut key,
                        None
                    ),
                    ERROR_SUCCESS
                );
                let _ = RegCloseKey(key);
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let mut monitor = monitors(false, &HashSet::new()).into_iter().next().expect("interactive display");
            monitor.name = "undo-test-display".into();
            let size = dip_size(&monitor);
            let gpu = Gpu::new().expect("Direct3D device");
            let simulation = SnowSimulation::new(size.0 as f32, size.1 as f32, 0.0);
            let mut screen = match Screen::new(&gpu, monitor, simulation, &crate::prefs::Settings::load(), None) {
                Ok(screen) => screen,
                Err((error, _)) => panic!("create test screen: {error}"),
            };
            let x = size.0 as f32 / 2.0;
            let base = V2::new(x, screen.simulation.ground_surface(x));
            assert!(screen.trees.objects.restore_with_appearance(base, vec![30.0; 3], Some((0, x)), 1));
            let mut gift = crate::gifts::Gift::with_kind(crate::gifts::GiftKind::Lantern, crate::gifts::GiftColor::Red, x, base.y + 50.0, 16.0, 0, x);
            gift.revealed = true;
            gift.attachment = Some((1, 1));
            screen.gifts.push(gift);
            let tree = crate::snow::CollectionObject::Tree(1, crate::cone::Species::Spruce, 0);
            assert_eq!(screen.move_collection_item(tree, true), Ok(1));
            assert!(screen.gifts[0].stored);
            assert!(screen.trees.objects.stacks().is_empty());
            assert_eq!(screen.undo_collection_move(), Ok(()));
            assert!(!screen.gifts[0].stored);
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));
            assert_eq!(screen.trees.objects.stacks()[0].appearance, 1);
            assert_eq!(screen.trees.objects.stacks()[0].base.x, x);
            let saved = crate::prefs::sculptures("boomboxes:undo-test-display").unwrap().unwrap();
            assert!(matches!(crate::gifts::decode_saved(&saved).unwrap()[0].support, crate::gifts::SavedSupport::Tree(1, 1)));
            let restored = crate::sculpt::Sculpt::trees("undo-test-display", size.0 as f32, size.1 as f32);
            assert!(restored.objects.stacks().iter().any(|tree| tree.appearance == 1));

            // A tree dragged to the collection returns to its origin before storage,
            // so undo and save failures preserve its original support and decoration.
            assert!(screen.trees.objects.pick_up_whole_where(base, |stack| stack.appearance == 1));
            screen.tree_origins.insert(1, (base, Some((0, x))));
            screen.trees.objects.drag(base + V2::new(80.0, 120.0));
            assert_eq!(screen.store_dragged_tree(), Ok(1));
            assert_eq!(screen.undo_collection_move(), Ok(()));
            assert_eq!(screen.trees.objects.stacks()[0].base, base);
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));
            assert!(screen.trees.objects.pick_up_whole_where(base, |stack| stack.appearance == 1));
            screen.tree_origins.insert(1, (base, Some((0, x))));
            screen.trees.objects.drag(base + V2::new(80.0, 120.0));
            screen.gifts_writable = false;
            assert_eq!(screen.store_dragged_tree(), Err(crate::snow::CollectionError::SaveFailed));
            assert_eq!(screen.trees.objects.stacks()[0].base, base);
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));
            screen.gifts_writable = true;

            let original = screen.gifts[0].clone();
            screen.gift_drag = Some(GiftDrag { index: 0, start: base, offset: V2::ZERO, moved: true, original: original.clone() });
            screen.gift_pressed = true;
            screen.gifts[0].begin_fall();
            assert_eq!(screen.store_dragged_gift(), Ok(0));
            assert!(screen.gifts[0].stored);
            assert_eq!(screen.undo_collection_move(), Ok(()));
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));
            screen.gift_drag = Some(GiftDrag { index: 0, start: base, offset: V2::ZERO, moved: true, original });
            screen.gift_pressed = true;
            screen.gifts[0].begin_fall();
            screen.gifts_writable = false;
            assert_eq!(screen.store_dragged_gift(), Err(crate::snow::CollectionError::SaveFailed));
            assert!(!screen.gifts[0].stored);
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));
            screen.gifts_writable = true;

            screen.gifts_writable = false;
            assert_eq!(screen.move_collection_item(tree, true), Err(crate::snow::CollectionError::SaveFailed));
            assert!(!screen.gifts[0].stored);
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));
            assert_eq!(screen.trees.objects.stacks()[0].appearance, 1);
            screen.gifts_writable = true;
            assert_eq!(screen.move_collection_item(tree, true), Ok(1));
            screen.gifts_writable = false;
            assert_eq!(screen.undo_collection_move(), Err(crate::snow::CollectionError::SaveFailed));
            assert!(screen.gifts[0].stored);
            assert!(screen.trees.objects.stacks().is_empty());
            screen.gifts_writable = true;
            screen.gifts[0].stored = false;
            assert_eq!(screen.undo_collection_move(), Err(crate::snow::CollectionError::Unavailable));
            assert!(screen.trees.objects.stacks().is_empty());
            screen.gifts[0].stored = true;
            assert!(screen.trees.objects.restore_with_appearance(base, vec![30.0; 3], Some((0, x)), 2));
            assert_eq!(screen.undo_collection_move(), Err(crate::snow::CollectionError::NoRoom));
            assert!(screen.gifts[0].stored);
            screen.trees.objects.take_resting(2).unwrap();
            assert_eq!(screen.undo_collection_move(), Ok(()));
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));
            screen.gift_pressed = true;
            assert_eq!(screen.move_collection_item(tree, true), Err(crate::snow::CollectionError::Busy));
            screen.gift_pressed = false;
            let toy = crate::snow::CollectionObject::Gift(crate::gifts::GiftKind::Lantern);
            assert_eq!(screen.move_collection_item(toy, true), Ok(0));
            assert!(screen.gifts[0].stored);
            assert_eq!(screen.undo_collection_move(), Ok(()));
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));

            // Exercise the actual mouse-up and capture-lost events against an isolated collection window.
            crate::inventory::show(crate::inventory::Handlers {
                snapshot: Box::new(Vec::new),
                height_in_feet: false,
                set_height_in_feet: Box::new(|_| {}),
                store: Box::new(|_| Ok(0)),
                undo: Box::new(|_| Ok(())),
                place: Box::new(|_, _| Ok(())),
            })
            .expect("show test collection");
            let window = crate::inventory::test_window();
            let mut bounds = RECT::default();
            unsafe {
                SetWindowPos(window, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE).expect("show test target");
                if let Some(other) = monitors(false, &HashSet::new()).into_iter().find(|monitor| monitor.rect != screen.monitor.rect) {
                    SetWindowPos(window, None, other.rect.left + 20, other.rect.top + 20, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)
                        .expect("move target to another display");
                }
                GetWindowRect(window, &mut bounds).expect("test collection bounds");
            }
            let drop_point = POINT { x: (bounds.left + bounds.right) / 2, y: (bounds.top + bounds.bottom) / 2 };
            assert!(crate::inventory::drop_target_at(drop_point));
            assert!(screen.trees.objects.pick_up_whole_where(base, |stack| stack.appearance == 1));
            screen.tree_origins.insert(1, (base, Some((0, x))));
            let input = screen.interaction_window().0 as isize;
            SCULPT_INPUT.with_borrow_mut(|inputs| inputs.entry(input).or_default().events.extend([(false, Some(drop_point)), (false, None)]));
            let clock = Clock::new();
            let tracker = crate::tracker::Tracker::new();
            screen.frame(&gpu, &clock, Clock::now(), PowerBudget::Full, Sky::now(false), 0.0, &tracker, true).expect("drop tree frame");
            assert!(screen.trees.objects.stacks().is_empty());
            assert!(screen.gifts[0].stored);
            unsafe {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, Some(window), 0, 0, PM_REMOVE).as_bool() {
                    let _ = DispatchMessageW(&message);
                }
            }
            let (_, message, undo_display) = crate::inventory::test_drop_feedback().expect("collection feedback");
            assert!(message.is_some());
            assert_eq!(undo_display.as_deref(), Some("undo-test-display"));
            assert_eq!(screen.undo_collection_move(), Ok(()));

            let original = screen.gifts[0].clone();
            screen.gift_drag = Some(GiftDrag { index: 0, start: base, offset: V2::ZERO, moved: true, original });
            screen.gift_pressed = true;
            screen.gifts[0].begin_fall();
            SCULPT_INPUT.with_borrow_mut(|inputs| inputs.entry(input).or_default().events.extend([(false, Some(drop_point)), (false, None)]));
            screen.frame(&gpu, &clock, Clock::now(), PowerBudget::Full, Sky::now(false), 0.0, &tracker, true).expect("drop gift frame");
            assert!(screen.gifts[0].stored);
            assert_eq!(screen.undo_collection_move(), Ok(()));
            assert_eq!(screen.gifts[0].attachment, Some((1, 1)));
            crate::inventory::close_test_window();
            return;
        }

        let path = format!("Software\\SnowhushInventoryUndoTest-{}", std::process::id());
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let mut key = HKEY::default();
        unsafe {
            assert_eq!(
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(wide.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_ALL_ACCESS,
                    None,
                    &mut key,
                    None
                ),
                ERROR_SUCCESS
            );
            let _ = RegCloseKey(key);
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["screen::collection_drop_tests::undo_restores_tree_and_attached_gift", "--exact", "--ignored", "--nocapture"])
            .env(CHILD, &path)
            .output()
            .unwrap();
        let removed = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wide.as_ptr())) };
        assert_eq!(removed, ERROR_SUCCESS);
        assert!(
            output.status.success(),
            "isolated test failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

pub fn dip_size(monitor: &Monitor) -> (u32, u32) {
    let (width, height) = physical_size(monitor);
    let scale = monitor.dpi.max(96) as f32 / 96.0;
    ((width as f32 / scale).ceil() as u32, (height as f32 / scale).ceil() as u32)
}

fn physical_size(monitor: &Monitor) -> (i32, i32) {
    let rect = &monitor.rect;
    ((rect.right - rect.left).max(1), (rect.bottom - rect.top).max(1))
}

/// Wakes the snow loop through `wake` the moment a taskbar that hides itself comes over an overlay, as it does when
/// clicked. The loop doesn't read its messages between frames and would only raise the overlay at the next one, long
/// enough for the drift to blink out under the taskbar, so the windows' stacking is watched from a thread of its own.
pub fn watch_taskbar(wake: isize) {
    static WAKE: AtomicIsize = AtomicIsize::new(0);
    unsafe extern "system" fn on_restack(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
        if overlays_over_hiding_taskbars().any(|(overlay, bounds)| taskbar_above(overlay, &bounds)) {
            let _ = unsafe { SetEvent(HANDLE(WAKE.load(Ordering::Relaxed) as _)) };
        }
    }
    WAKE.store(wake, Ordering::Relaxed);
    let _ = std::thread::Builder::new().name("taskbar".into()).spawn(|| unsafe {
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let hooks = [EVENT_SYSTEM_FOREGROUND, EVENT_OBJECT_REORDER].map(|event| SetWinEventHook(event, event, None, Some(on_restack), 0, 0, flags));
        if hooks.iter().all(|hook| hook.is_invalid()) {
            return;
        }
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            DispatchMessageW(&message);
        }
    });
}

/// The overlays above windows that reach over their display's taskbar, as on displays whose taskbar hides itself and
/// while it rains, with each display's bounds. The hidden listener has the same class, but it isn't click-through.
fn overlays_over_hiding_taskbars() -> impl Iterator<Item = (HWND, RECT)> {
    let next = |previous: Option<HWND>| unsafe { FindWindowExW(None, previous, CLASS, None) }.ok();
    std::iter::successors(next(None), move |&window| next(Some(window))).filter_map(|window| unsafe {
        if GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0 == 0 {
            return None;
        }
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let found = GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut info).as_bool();
        let mut rect = RECT::default();
        let reaches = GetWindowRect(window, &mut rect).is_ok() && rect.bottom >= info.rcMonitor.bottom;
        (found && (info.rcWork == info.rcMonitor || reaches)).then_some((window, info.rcMonitor))
    })
}

/// Only the relative order of trees and snow sculptures changes; other objects remain in front.
fn order_sculptures<'a>(
    sculptures: &'a [SculptureDraw],
    snow_count: usize,
    trees: &'a [SculptureDraw],
    together: bool,
    trees_in_front: bool,
    separate_controls: bool,
) -> Vec<&'a SculptureDraw> {
    let mut result = Vec::new();
    if together && !trees_in_front {
        result.extend(trees.iter());
    }
    result.extend(sculptures[..snow_count].iter().filter(|group| !separate_controls || !group.control));
    if together && trees_in_front {
        result.extend(trees.iter());
    }
    result.extend(sculptures[snow_count..].iter().filter(|group| !separate_controls || !group.control));
    result
}

/// The lowest of the menus, tooltips and shell flyouts open at the very top, which the snow goes just under:
/// menus and tooltips of any app, and any window of the shell's but its taskbars, since the shell's menus
/// come in classes that change between versions of Windows.
/// Only those above every other window count: some of the shell's stay showing further down, under the taskbar
/// among others, and going under one of those would leave the snow under the taskbar too. Click-through windows
/// are looked past: they are overlays like this one, such as AMD's, which keeps itself above everything.
fn lowest_open_popup() -> Option<HWND> {
    let shell = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.map_or(0, |taskbar| {
        let mut process = 0;
        unsafe { GetWindowThreadProcessId(taskbar, Some(&mut process)) };
        process
    });
    let mut lowest = None;
    let mut top = Vec::new();
    let mut next = unsafe { GetTopWindow(None) }.ok();
    while let Some(window) = next {
        let extended = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) } as u32;
        if extended & WS_EX_TOPMOST.0 == 0 {
            break;
        }
        if extended & WS_EX_TRANSPARENT.0 == 0 && unsafe { IsWindowVisible(window) }.as_bool() && !cloaked(window) {
            let class = class_name(window);
            let mut process = 0;
            unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
            if class == "SnowhushOverlay" && process == std::process::id() {
                next = unsafe { GetWindow(window, GW_HWNDNEXT) }.ok();
                continue;
            }
            let popup = class == "#32768"
                || class == "tooltips_class32"
                || class == "SnowhushMenu"
                || class == "SnowhushInventory"
                || class == "SnowhushInventoryDrag"
                || COVER_ONLY.contains(&class.as_str())
                || (process == shell && class != "Shell_TrayWnd" && class != "Shell_SecondaryTrayWnd");
            top.push((class, process));
            if !popup {
                break;
            }
            lowest = Some(window);
        }
        next = unsafe { GetWindow(window, GW_HWNDNEXT) }.ok();
    }
    TOP_LOGGED.with_borrow_mut(|logged| {
        let seen = format!("{top:?} {}", lowest.is_some());
        if *logged != seen {
            let names: Vec<String> = top.iter().map(|(class, process)| format!("{class} of {}", process_name(*process))).collect();
            log!("top windows: {}; snow {}", names.join(", "), if lowest.is_some() { "goes under the last menu" } else { "goes on top" });
            *logged = seen;
        }
    });
    lowest
}

/// Whether a behind-windows overlay must move: something other than our own overlays sits above it
/// before the desktop icons (a live wallpaper, a stray WorkerW, …), or the icons are not above it at all.
fn behind_needs_raise(overlay: HWND, icons: Option<HWND>) -> bool {
    unsafe {
        match icons {
            Some(icons) => {
                let mut window = overlay;
                while let Ok(above) = GetWindow(window, GW_HWNDPREV) {
                    if above == icons {
                        return false;
                    }
                    if !class_name(above).encode_utf16().eq(CLASS.as_wide().iter().copied()) {
                        return true;
                    }
                    window = above;
                }
                true
            }
            None => {
                let mut window = overlay;
                while let Ok(above) = GetWindow(window, GW_HWNDPREV) {
                    if !class_name(above).encode_utf16().eq(CLASS.as_wide().iter().copied()) {
                        return true;
                    }
                    window = above;
                }
                false
            }
        }
    }
}

/// Whether the taskbar on the display with these bounds stands above the overlay.
fn taskbar_above(overlay: HWND, bounds: &RECT) -> bool {
    let Some(taskbar) = taskbar(bounds) else {
        return false;
    };
    let mut window = overlay;
    unsafe {
        while let Ok(above) = GetWindow(window, GW_HWNDPREV) {
            if above == taskbar {
                return true;
            }
            window = above;
        }
    }
    false
}

/// The taskbar's ledge while it rains, apart from any window's.
const TASKBAR: WindowId = -2;

/// The taskbar on the display with these bounds, if it has one.
fn taskbar(bounds: &RECT) -> Option<HWND> {
    unsafe {
        let primary = FindWindowW(w!("Shell_TrayWnd"), None).ok();
        let secondary = std::iter::successors(FindWindowExW(None, None, w!("Shell_SecondaryTrayWnd"), None).ok(), |&previous| {
            FindWindowExW(None, Some(previous), w!("Shell_SecondaryTrayWnd"), None).ok()
        });
        primary.into_iter().chain(secondary).find(|&window| {
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            GetMonitorInfoW(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() && info.rcMonitor == *bounds
        })
    }
}

fn cone_input_rect(bounds: &RECT, size: (u32, u32), centre: V2) -> RECT {
    let sx = (bounds.right - bounds.left) as f32 / size.0.max(1) as f32;
    let sy = (bounds.bottom - bounds.top) as f32 / size.1.max(1) as f32;
    RECT {
        left: bounds.left + ((centre.x - HIT_RADIUS) * sx).floor() as i32,
        right: bounds.left + ((centre.x + HIT_RADIUS) * sx).ceil() as i32,
        top: bounds.bottom - ((centre.y + HIT_RADIUS) * sy).ceil() as i32,
        bottom: bounds.bottom - ((centre.y - HIT_RADIUS) * sy).floor() as i32,
    }
}

fn take_collectible_click(window: HWND) -> bool {
    SCULPT_INPUT.with_borrow_mut(|inputs| {
        inputs
            .get_mut(&(window.0 as isize))
            .map(|input| std::mem::take(&mut input.events))
            .is_some_and(|events| events.into_iter().any(|(down, point)| down && point.is_some()))
    })
}

fn collect_cone_click(window: HWND, cone: &mut Option<Cone>, blocked: bool) -> bool {
    let clicked = take_collectible_click(window);
    if clicked
        && !blocked
        && let Some(cone) = cone
        && !cone.collected
    {
        cone.collected = true;
        return true;
    }
    false
}

/// An almost invisible, nonactivating input surface; its footprint is only the cone's hit area.
fn create_cone_input(rect: &RECT, shown_in_screenshots: bool) -> Result<HWND> {
    register_class()?;
    unsafe {
        let window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("VelvetFrost"),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        if let Err(error) = SetLayeredWindowAttributes(window, COLORREF(0), 1, LWA_ALPHA) {
            let _ = DestroyWindow(window);
            return Err(error);
        }
        set_capture(window, shown_in_screenshots);
        let _ = SetPropW(window, w!("NonRudeHWND"), Some(HANDLE(1 as *mut c_void)));
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        Ok(window)
    }
}

fn create_overlay(rect: &RECT, shown_in_screenshots: bool) -> Result<HWND> {
    register_class()?;
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let window = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("VelvetFrost"),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        // A layered window stays invisible until it is given an opacity; WS_EX_TRANSPARENT then lets clicks through.
        SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA)?;
        set_capture(window, shown_in_screenshots);
        // With the taskbar set to hide itself, the overlay fills the whole display, and the shell would take it for a
        // full-screen app: notifications go quiet, and a maximized window in front reads as full screen and covers it.
        let _ = SetPropW(window, w!("NonRudeHWND"), Some(HANDLE(1 as *mut c_void)));
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        Ok(window)
    }
}

/// A child of the wallpaper layer, under the icons, so it needs neither click-through nor a place above other windows.
/// Screenshots can't leave it out: only top-level windows can be kept from capture.
fn create_behind(rect: &RECT, layer: &DesktopLayer) -> Result<HWND> {
    register_class()?;
    unsafe {
        let mut origin = POINT { x: rect.left, y: rect.top };
        let _ = ScreenToClient(layer.parent, &mut origin);
        let window = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            CLASS,
            w!("VelvetFrost"),
            WS_CHILD | WS_VISIBLE | WS_DISABLED,
            origin.x,
            origin.y,
            rect.right - rect.left,
            rect.bottom - rect.top,
            Some(layer.parent),
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        // Unlayered, a child counts as opaque, and siblings under it stop drawing: Lively's wallpaper among them.
        SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA)?;
        if let Some(icons) = layer.icons {
            let _ = SetWindowPos(window, Some(icons), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        Ok(window)
    }
}

fn register_class() -> Result<()> {
    static REGISTER: Once = Once::new();
    let instance = unsafe { GetModuleHandleW(None)? };
    REGISTER.call_once(|| {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(overlay_procedure),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        unsafe { RegisterClassExW(&class) };
    });
    Ok(())
}

fn set_capture(window: HWND, shown: bool) {
    unsafe {
        let _ = SetWindowDisplayAffinity(window, if shown { WDA_NONE } else { WDA_EXCLUDEFROMCAPTURE });
    }
}

unsafe extern "system" fn overlay_procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => {
            let x = (lparam.0 as u32 & 0xffff) as u16 as i16 as i32;
            let y = ((lparam.0 as u32 >> 16) & 0xffff) as u16 as i16 as i32;
            if crate::inventory::drop_target_at(POINT { x, y }) {
                return LRESULT(HTTRANSPARENT as isize);
            }
            let captures = SCULPT_INPUT.with_borrow(|inputs| {
                inputs.get(&(window.0 as isize)).is_some_and(|state| {
                    let inside = |r: RECT| {
                        let rx = (r.right - r.left).max(1) as f32 * 0.5;
                        let ry = (r.bottom - r.top).max(1) as f32 * 0.5;
                        let dx = (x as f32 - (r.left + r.right) as f32 * 0.5) / rx;
                        let dy = (y as f32 - (r.top + r.bottom) as f32 * 0.5) / ry;
                        dx * dx + dy * dy <= 1.0
                    };
                    state.captures && (state.active || state.icon.is_some_and(inside) || state.balls.iter().copied().any(inside))
                })
            });
            LRESULT(if captures { HTCLIENT as isize } else { HTTRANSPARENT as isize })
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let track = SCULPT_INPUT.with_borrow_mut(|inputs| {
                inputs.get_mut(&(window.0 as isize)).is_some_and(|state| {
                    if !state.is_cone {
                        return false;
                    }
                    let track = !state.hovered;
                    state.hovered = true;
                    track
                })
            });
            if track {
                let mut event =
                    TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: window, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut event);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            SCULPT_INPUT.with_borrow_mut(|inputs| {
                if let Some(state) = inputs.get_mut(&(window.0 as isize)) {
                    state.hovered = false;
                }
            });
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 == SCULPT_ESCAPE as usize => {
            SCULPT_INPUT.with_borrow_mut(|inputs| {
                if let Some(state) = inputs.get_mut(&(window.0 as isize)) {
                    state.events.push((true, None));
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP => {
            let down = message == WM_LBUTTONDOWN;
            // Mouse messages carry the position at click time. The current cursor can already
            // have moved, and GetCursorPos may be unavailable in some desktop sessions.
            let mut point = POINT { x: (lparam.0 as u32 & 0xffff) as u16 as i16 as i32, y: ((lparam.0 as u32 >> 16) & 0xffff) as u16 as i16 as i32 };
            let point = unsafe { ClientToScreen(window, &mut point) }.as_bool().then_some(point);
            SCULPT_INPUT.with_borrow_mut(|inputs| {
                if let Some(state) = inputs.get_mut(&(window.0 as isize)) {
                    state.events.push((down, point));
                }
            });
            unsafe {
                if down {
                    SetCapture(window);
                } else {
                    let _ = ReleaseCapture();
                }
            }
            LRESULT(0)
        }
        WM_CANCELMODE | WM_CAPTURECHANGED => {
            SCULPT_INPUT.with_borrow_mut(|inputs| {
                if let Some(state) = inputs.get_mut(&(window.0 as isize)) {
                    state.events.push((false, None));
                }
            });
            unsafe {
                let _ = ReleaseCapture();
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            DISPLAYS_CHANGED.set(true);
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wparam.0 as u32 == SPI_SETWORKAREA.0 => {
            DISPLAYS_CHANGED.set(true);
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wparam.0 as u32 == SPI_SETDESKWALLPAPER.0 => {
            BACKDROP_CHANGED.set(true);
            LRESULT(0)
        }
        WM_SYSCOLORCHANGE => {
            BACKDROP_CHANGED.set(true);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod sculpt_input_tests {
    use super::*;
    use crate::sculpt::SculptureDraw;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    #[test]
    fn tree_and_snowman_order_changes_without_reordering_other_objects() {
        let make = |control| SculptureDraw { sprites: Vec::new(), covers: Vec::new(), control };
        let sculptures = [make(false), make(true), make(false)];
        let trees = [make(false)];
        let check = |together, front, controls, expected: &[&SculptureDraw]| {
            let actual = order_sculptures(&sculptures, 2, &trees, together, front, controls);
            assert_eq!(actual.len(), expected.len());
            assert!(actual.iter().zip(expected).all(|(a, b)| std::ptr::eq(*a, *b)));
        };
        check(true, false, false, &[&trees[0], &sculptures[0], &sculptures[1], &sculptures[2]]);
        check(true, true, false, &[&sculptures[0], &sculptures[1], &trees[0], &sculptures[2]]);
        check(true, false, true, &[&trees[0], &sculptures[0], &sculptures[2]]);
        check(false, false, false, &[&sculptures[0], &sculptures[1], &sculptures[2]]);
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn tree_and_snowman_priority_changes_overlapping_pixels() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 200, bottom: 200 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 200, 200, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(200.0, 200.0, 0.0);
        let mut objects = snowhush_core::snowmen::Snowmen::default();
        assert!(objects.restore_with_appearance(V2::new(100.0, 0.0), vec![28.0, 22.0, 17.0], Some((0, 100.0)), 1));
        let snowman = SculptureDraw { sprites: crate::sculpt::sculpture_sprites(&objects.stacks()[0], 0.0), covers: Vec::new(), control: false };
        let mut objects = snowhush_core::snowmen::Snowmen::default();
        assert!(objects.restore_with_appearance(V2::new(100.0, 0.0), vec![28.0; 3], Some((0, 100.0)), 2));
        let tree = SculptureDraw {
            sprites: crate::sculpt::tree_sprites(&objects.stacks()[0], crate::cone::Species::Spruce, 0, 0.0),
            covers: Vec::new(),
            control: false,
        };
        let sculptures = [snowman];
        let trees = [tree];
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let mut draw = |front| {
            surface.test_capture = Some(Vec::new());
            let groups = order_sculptures(&sculptures, 1, &trees, true, front, false);
            surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &groups, true).expect("draw overlapping objects");
            surface.test_capture.take().expect("read rendered pixels")
        };
        let snowman_front = draw(false);
        let tree_front = draw(true);
        let changed = snowman_front.chunks_exact(4).zip(tree_front.chunks_exact(4)).filter(|(a, b)| a != b).count();
        assert!(changed > 100, "priority must visibly change the overlap, got {changed} pixels");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn glass_controls_render_distinct_symbols_on_translucent_surfaces() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 144, bottom: 48 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 144, 48, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(144.0, 48.0, 0.0);
        let groups: Vec<_> = [(false, false), (false, true), (true, false)]
            .into_iter()
            .enumerate()
            .map(|(index, (tree, active))| SculptureDraw {
                sprites: vec![crate::sculpt::control_sprite(V2::new(24.0 + index as f32 * 48.0, 24.0), tree, active)],
                covers: Vec::new(),
                control: true,
            })
            .collect();
        let refs: Vec<_> = groups.iter().collect();
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &refs, false).expect("draw controls");
        let pixels = surface.test_capture.take().expect("read controls");
        let tile = |left: usize| -> Vec<u8> {
            let mut result = Vec::new();
            for y in 10..38 {
                result.extend_from_slice(&pixels[(y * 144 + left) * 4..(y * 144 + left + 28) * 4]);
            }
            result
        };
        let (start, finish, move_tree) = (tile(10), tile(58), tile(106));
        assert_ne!(start, finish);
        assert_ne!(start, move_tree);
        assert_ne!(finish, move_tree);
        assert!(start.chunks_exact(4).any(|pixel| (40..220).contains(&pixel[3])), "glass should be translucent");
        assert!(start.chunks_exact(4).any(|pixel| pixel[3] == 0), "the circular button needs transparent corners");
    }

    #[test]
    fn idle_icon_does_not_intercept_clicks_elsewhere() {
        let window = HWND(1 as _);
        SCULPT_INPUT.with_borrow_mut(|inputs| {
            inputs.insert(1, SculptInput { captures: true, icon: Some(RECT { left: 90, top: 90, right: 110, bottom: 110 }), ..Default::default() });
        });
        let location = |x: u16, y: u16| LPARAM((u32::from(x) | (u32::from(y) << 16)) as isize);
        assert_eq!(unsafe { overlay_procedure(window, WM_NCHITTEST, WPARAM(0), location(100, 100)) }.0, HTCLIENT as isize);
        assert_eq!(unsafe { overlay_procedure(window, WM_NCHITTEST, WPARAM(0), location(150, 150)) }.0, HTTRANSPARENT as isize);
        assert_eq!(unsafe { overlay_procedure(window, WM_NCHITTEST, WPARAM(0), location(90, 90)) }.0, HTTRANSPARENT as isize);
        SCULPT_INPUT.with_borrow_mut(|inputs| {
            inputs.remove(&1);
        });
    }

    #[test]
    fn idle_overlay_only_intercepts_clicks_on_a_snowball() {
        let window = HWND(1 as _);
        SCULPT_INPUT.with_borrow_mut(|inputs| {
            inputs.insert(1, SculptInput { captures: true, balls: vec![RECT { left: 90, top: 90, right: 110, bottom: 110 }], ..Default::default() });
        });
        let location = |x: u16, y: u16| LPARAM((u32::from(x) | (u32::from(y) << 16)) as isize);
        assert_eq!(unsafe { overlay_procedure(window, WM_NCHITTEST, WPARAM(0), location(100, 100)) }.0, HTCLIENT as isize);
        assert_eq!(unsafe { overlay_procedure(window, WM_NCHITTEST, WPARAM(0), location(90, 90)) }.0, HTTRANSPARENT as isize);
        assert_eq!(unsafe { overlay_procedure(window, WM_NCHITTEST, WPARAM(0), location(150, 150)) }.0, HTTRANSPARENT as isize);
        SCULPT_INPUT.with_borrow_mut(|inputs| {
            inputs.remove(&1);
        });
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn transparent_overlay_can_receive_sculpting_clicks() {
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        assert!(width > 100 && height > 100);
        let rect = RECT { left: width - 8, top: 0, right: width, bottom: 8 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        SCULPT_INPUT.with_borrow_mut(|inputs| {
            inputs.insert(
                overlay.0.0 as isize,
                SculptInput { captures: true, balls: vec![RECT { left: width - 8, top: 0, right: width, bottom: 8 }], ..Default::default() },
            );
        });
        unsafe {
            let style = GetWindowLongPtrW(overlay.0, GWL_EXSTYLE) & !(WS_EX_TRANSPARENT.0 as isize);
            SetWindowLongPtrW(overlay.0, GWL_EXSTYLE, style);
            let hit = WindowFromPoint(POINT { x: width - 4, y: 4 });
            assert_eq!(hit, overlay.0, "the overlay must receive a snowball click outside sculpting mode");
            RegisterHotKey(Some(overlay.0), SCULPT_ESCAPE, MOD_NOREPEAT, VK_ESCAPE.0 as u32).expect("reserve Escape while sculpting");
            UnregisterHotKey(Some(overlay.0), SCULPT_ESCAPE).expect("release Escape after sculpting");
        }
    }

    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn cone_input_window_receives_clicks_without_covering_the_rest_of_the_screen() {
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        assert!(width > 200 && height > 200);
        let rect = RECT { left: width / 2 - 30, top: height / 2 - 30, right: width / 2 + 30, bottom: height / 2 + 30 };
        let window = Overlay(create_cone_input(&rect, true).expect("create cone input window"));
        SCULPT_INPUT.with_borrow_mut(|inputs| {
            inputs.insert(window.0.0 as isize, SculptInput { captures: true, is_cone: true, balls: vec![rect], ..Default::default() });
        });
        let point = POINT { x: width / 2, y: height / 2 };
        let hit = unsafe { WindowFromPoint(point) };
        assert_eq!(hit, window.0, "Windows must route a click at the cone to its input window");
        let near = POINT { x: point.x + 27, y: point.y };
        assert_eq!(unsafe { WindowFromPoint(near) }, window.0, "the margin around the cone must also accept clicks");
        assert_ne!(unsafe { WindowFromPoint(POINT { x: rect.right + 10, y: point.y }) }, window.0);
        unsafe {
            SendMessageW(hit, WM_LBUTTONDOWN, None, None);
            SendMessageW(hit, WM_LBUTTONUP, None, None);
        }
        let mut cone = Some(Cone::new(100.0, 100.0, 0));
        assert!(collect_cone_click(window.0, &mut cone, false), "the click must be consumed by the cone");
        assert!(cone.unwrap().collected);
        unsafe {
            SendMessageW(hit, WM_MOUSEMOVE, None, None);
        }
        assert!(SCULPT_INPUT.with_borrow(|inputs| inputs.get(&(window.0.0 as isize)).unwrap().hovered));
        unsafe {
            SendMessageW(hit, WM_MOUSELEAVE, None, None);
        }
        assert!(!SCULPT_INPUT.with_borrow(|inputs| inputs.get(&(window.0.0 as isize)).unwrap().hovered));
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn road_train_both_sections_render_in_both_directions() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 640, bottom: 200 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 640, 200, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(640.0, 200.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let group = SculptureDraw { sprites: vec![Sprite { alpha: 0.0, ..Sprite::default() }], covers: Vec::new(), control: false };
        for right in [true, false] {
            let mut truck = crate::truck::Truck::new_variant(640.0, 1920, 1080, right, true);
            truck.sprite_width = 300.0;
            truck.x = if right { 380.0 } else { 260.0 };
            truck.follow_terrain(1.0, |_| 25.0);
            surface.test_capture = Some(Vec::new());
            surface.draw(&gpu, &simulation, ambient, 0.0, None, Some(&truck), None, &[&group], false).expect("draw road train");
            let pixels = surface.test_capture.take().expect("read rendered pixels");
            for half in 0..2 {
                let red = pixels
                    .chunks_exact(4)
                    .enumerate()
                    .filter(|(i, p)| (*i % 640) / 320 == half && p[2] > 90 && u16::from(p[2]) > u16::from(p[1]) * 2 && p[3] > 100)
                    .count();
                assert!(red > 500, "both road train sections should render, direction {right}, half {half}: {red}");
            }
        }
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn cone_texture_and_hover_halo_render_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 128, bottom: 128 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 128, 128, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(128.0, 128.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let render = |surface: &mut Surface, hover| {
            let group =
                SculptureDraw { sprites: cone_sprites(V2::new(64.0, 64.0), hover, crate::cone::Species::Spruce), covers: Vec::new(), control: false };
            surface.test_capture = Some(Vec::new());
            surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw cone");
            surface.test_capture.take().expect("read rendered pixels")
        };
        let idle = render(&mut surface, false);
        let hovered = render(&mut surface, true);
        let brown = idle.chunks_exact(4).filter(|p| p[2] > p[1] && p[1] > p[0] && p[3] > 200).count();
        let halo = |pixels: &[u8]| -> u64 {
            pixels
                .chunks_exact(4)
                .enumerate()
                .filter(|(i, _)| {
                    let x = (*i % 128) as i32 - 64;
                    let y = (*i / 128) as i32 - 64;
                    x * x + y * y > 22 * 22
                })
                .map(|(_, pixel)| u64::from(pixel[3]))
                .sum()
        };
        assert!(brown > 200, "pine cone body must render");
        assert!(halo(&hovered) > halo(&idle) * 3 / 2, "hover must brighten the halo");
        if let Some(path) = std::env::var_os("SNOWHUSH_CONE_IMAGE") {
            std::fs::write(path, &hovered).expect("write BGRA pixels");
        }
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn gift_texture_renders_with_transparency_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 128, bottom: 128 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 128, 128, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(128.0, 128.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let mut group = SculptureDraw {
            sprites: vec![Sprite { position: V2::new(64.0, 64.0), size: 42.0, alpha: 1.0, mood: [15.0, 0.0], ..Sprite::default() }],
            covers: Vec::new(),
            control: false,
        };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw gift");
        let pixels = surface.test_capture.take().expect("read rendered pixels");
        assert!(pixels.chunks_exact(4).filter(|p| p[2] > p[1] && p[2] > p[0] && p[3] > 180).count() > 300, "gift should be visible and red");
        assert!(pixels.chunks_exact(4).filter(|p| p[3] == 0).count() > 10_000, "gift background should be transparent");
        group.sprites[0].pose[0] = 0.5;
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw opening gift");
        let opening = surface.test_capture.take().expect("read opening pixels");
        assert_ne!(pixels, opening, "the box must change shape while opening");
        group.sprites[0].pose = [0.0, 1.0, 0.0, 0.0];
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw green gift");
        let green = surface.test_capture.take().expect("read green gift pixels");
        assert!(green.chunks_exact(4).filter(|p| p[1] > p[2] && p[1] > p[0] && p[3] > 180).count() > 100, "green gift should be visible");
        group.sprites[0].mood = [16.0, 0.2];
        group.sprites[0].pose = [0.0; 4];
        group.sprites[0].size = 70.0;
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw boombox");
        let boombox = surface.test_capture.take().expect("read boombox pixels");
        assert!(boombox.chunks_exact(4).filter(|p| p[3] > 180).count() > 500, "boombox should be visible");
        group.sprites[0].mood = [18.0, 0.0];
        group.sprites[0].pose[1] = 1.0;
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw lit lantern");
        let lit = surface.test_capture.take().expect("read lit lantern pixels");
        group.sprites[0].pose[1] = 0.0;
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw unlit lantern");
        let unlit = surface.test_capture.take().expect("read unlit lantern pixels");
        let brightness = |data: &[u8]| -> u64 { data.chunks_exact(4).map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum() };
        assert!(lit.chunks_exact(4).filter(|p| p[3] > 180).count() > 300, "lantern should be visible");
        assert!(
            brightness(&lit) * 100 > brightness(&unlit) * 105,
            "switching off must dim the lantern: lit {}, unlit {}",
            brightness(&lit),
            brightness(&unlit)
        );
        assert!(brightness(&unlit) * 2 > brightness(&lit), "the unlit lantern must retain its visible frame");
        let ember = unlit.chunks_exact(4).enumerate().any(|(index, p)| {
            let (x, y) = (index % 128, index / 128);
            (62..=66).contains(&x) && (74..=78).contains(&y) && p[2] > 100 && p[2] > p[1].saturating_add(30)
        });
        assert!(ember, "the switched-off lantern must retain a warm ember");
        let night = Ambient { light: [0.3; 3], ..ambient };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, night, 0.0, None, None, None, &[&group], false).expect("draw unlit lantern at night");
        let dark = surface.test_capture.take().expect("read night lantern pixels");
        assert!(brightness(&dark) * 2 > brightness(&unlit), "the unlit frame must remain legible at night");
        assert!(
            dark.chunks_exact(4).enumerate().any(|(index, p)| {
                let (x, y) = (index % 128, index / 128);
                (62..=66).contains(&x) && (74..=78).contains(&y) && p[2] > 70 && p[2] > p[1].saturating_add(20)
            }),
            "the ember must remain visible at night"
        );
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn enabled_lantern_lights_nearby_snow_on_gpu() {
        use snowhush_core::simulation::ScatterPiece;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 128, bottom: 128 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 128, 128, (1.0, 1.0)).expect("create drawing surface");
        let mut simulation = SnowSimulation::new(128.0, 128.0, 0.0);
        let pieces: Vec<_> = (0..6000)
            .map(|i| ScatterPiece { position: V2::new(5.0 + (i % 118) as f32, 10.0), velocity: V2::new(0.0, -80.0), color: [1.0; 3] })
            .collect();
        simulation.scatter(&pieces);
        for _ in 0..120 {
            simulation.step(0.05, V2::new(-1000.0, -1000.0));
        }
        let mut group = SculptureDraw {
            sprites: vec![Sprite {
                position: V2::new(64.0, 40.0),
                size: 40.0,
                alpha: 1.0,
                mood: [18.0, 0.0],
                pose: [0.0, 1.0, 0.0, 0.0],
                ..Sprite::default()
            }],
            covers: Vec::new(),
            control: false,
        };
        let ambient = Ambient { light: [0.4; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], true).expect("draw lit snow");
        let lit = surface.test_capture.take().unwrap();
        group.sprites[0].pose[1] = 0.0;
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], true).expect("draw unlit snow");
        let unlit = surface.test_capture.take().unwrap();
        let red = |pixels: &[u8]| -> u64 { (110..128).flat_map(|y| (5..25).map(move |x| (y * 128 + x) * 4)).map(|i| u64::from(pixels[i + 2])).sum() };
        assert!(red(&lit) > red(&unlit) * 3 / 2, "the enabled lantern should illuminate nearby snow");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn boombox_is_visible_on_each_adult_tree_design() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 768, bottom: 800 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 768, 800, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(768.0, 800.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let mut trees = Vec::new();
        let mut decorations = Vec::new();
        for (index, species) in [crate::cone::Species::Spruce, crate::cone::Species::Pine, crate::cone::Species::Fir].into_iter().enumerate() {
            for variant in 0..4_u8 {
                let base = V2::new(130.0 + index as f32 * 250.0, 15.0 + f32::from(variant) * 195.0);
                let mut collection = snowhush_core::snowmen::Snowmen::default();
                assert!(collection.restore_with_appearance(base, vec![26.0; 3], Some((0, base.x)), 1));
                let tree = &collection.stacks()[0];
                let height = tree.height_from_radii();
                let centre = crate::ornaments::tree_slots(species, variant)[0].world_position(base, height);
                trees.push(SculptureDraw { sprites: crate::sculpt::tree_sprites(tree, species, variant, 0.0), covers: Vec::new(), control: false });
                decorations.push(SculptureDraw {
                    sprites: vec![Sprite {
                        position: centre,
                        size: (height * 0.26).clamp(26.0, 45.0) * 1.5,
                        alpha: 1.0,
                        mood: [16.0, 0.0],
                        ..Sprite::default()
                    }],
                    covers: Vec::new(),
                    control: false,
                });
            }
        }
        surface.test_capture = Some(Vec::new());
        let groups: Vec<_> = trees.iter().collect();
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &groups, false).expect("draw trees");
        let before = surface.test_capture.take().unwrap();
        surface.test_capture = Some(Vec::new());
        let groups: Vec<_> = trees.iter().chain(decorations.iter()).collect();
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &groups, false).expect("draw decorations");
        let after = surface.test_capture.take().unwrap();
        for decoration in &decorations {
            let p = decoration.sprites[0].position;
            let mut different = 0;
            for y in ((800.0 - p.y) as i32 - 25)..((800.0 - p.y) as i32 + 25) {
                for x in (p.x as i32 - 35)..(p.x as i32 + 35) {
                    let offset = (y as usize * 768 + x as usize) * 4;
                    if before[offset..offset + 4] != after[offset..offset + 4] {
                        different += 1;
                    }
                }
            }
            assert!(different > 200, "boombox must remain visible on every tree design");
        }
        if let Some(path) = std::env::var_os("SNOWHUSH_ATTACHMENT_IMAGE") {
            std::fs::write(path, after).expect("write BGRA pixels");
        }
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn celestial_tree_and_golden_snowflake_render_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 600, bottom: 400 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 600, 400, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(600.0, 400.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let mut collection = snowhush_core::snowmen::Snowmen::default();
        assert!(collection.restore_with_appearance(V2::new(300.0, 20.0), vec![30.0; 5], Some((0, 300.0)), 1));
        let tree = SculptureDraw {
            sprites: crate::sculpt::tree_sprites(&collection.stacks()[0], crate::cone::Species::Spruce, crate::sculpt::CELESTIAL_VARIANT, 0.0),
            covers: Vec::new(),
            control: false,
        };
        let star = SculptureDraw {
            sprites: vec![Sprite { position: V2::new(70.0, 300.0), size: 60.0, alpha: 1.0, mood: [17.0, 0.0], ..Sprite::default() }],
            covers: Vec::new(),
            control: false,
        };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&tree, &star], false).expect("draw celestial sprites");
        let pixels = surface.test_capture.take().expect("read pixels");
        let opaque_in = |left: usize, right: usize, top: usize, bottom: usize| -> usize {
            (top..bottom).flat_map(|y| (left..right).map(move |x| (y * 600 + x) * 4)).filter(|&p| pixels[p + 3] > 100).count()
        };
        assert!(opaque_in(180, 420, 80, 390) > 5_000, "the celestial tree must render");
        assert!(opaque_in(40, 100, 70, 130) > 300, "the golden snowflake must render");
        let bare_tree = SculptureDraw { sprites: vec![tree.sprites[0]], covers: Vec::new(), control: false };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&bare_tree, &star], false).expect("draw tree without lights");
        let without_lights = surface.test_capture.take().expect("read unlit pixels");
        assert!(
            pixels.chunks_exact(4).zip(without_lights.chunks_exact(4)).filter(|(a, b)| a != b).count() > 100,
            "the animated lights must be visible"
        );
        let ruby = SculptureDraw {
            sprites: crate::sculpt::tree_sprites(&collection.stacks()[0], crate::cone::Species::Spruce, crate::sculpt::RUBY_VARIANT, 0.0),
            covers: Vec::new(),
            control: false,
        };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&ruby], false).expect("draw ruby tree");
        let ruby_pixels = surface.test_capture.take().expect("read ruby pixels");
        assert!(ruby_pixels.chunks_exact(4).filter(|pixel| pixel[3] > 100).count() > 5_000, "the ruby tree must render");
        assert!(
            ruby_pixels.chunks_exact(4).zip(without_lights.chunks_exact(4)).filter(|(a, b)| a != b).count() > 5_000,
            "the legendary designs must be distinct"
        );
        let rose = SculptureDraw {
            sprites: crate::sculpt::tree_sprites(&collection.stacks()[0], crate::cone::Species::Spruce, crate::sculpt::ROSE_VARIANT, 0.0),
            covers: Vec::new(),
            control: false,
        };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&rose], false).expect("draw rose tree");
        let rose_pixels = surface.test_capture.take().expect("read rose pixels");
        assert!(rose_pixels.chunks_exact(4).filter(|pixel| pixel[3] > 100).count() > 5_000, "the rose tree must render");
        assert!(
            rose_pixels.chunks_exact(4).zip(ruby_pixels.chunks_exact(4)).filter(|(a, b)| a != b).count() > 5_000,
            "the rose and ruby designs must be distinct"
        );
        if let Some(path) = std::env::var_os("SNOWHUSH_CELESTIAL_IMAGE") {
            std::fs::write(path, pixels).expect("write BGRA pixels");
        }
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn bullfinch_poses_render_with_transparency_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 128, bottom: 128 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 128, 128, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(128.0, 128.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let mut group = SculptureDraw {
            sprites: vec![Sprite { position: V2::new(64.0, 64.0), size: bullfinch::SIZE, alpha: 1.0, mood: [20.0, 0.0], ..Sprite::default() }],
            covers: Vec::new(),
            control: false,
        };
        let mut frames = Vec::new();
        for frame in 0..2 {
            group.sprites[0].pose[0] = frame as f32;
            surface.test_capture = Some(Vec::new());
            surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw bullfinch");
            let pixels = surface.test_capture.take().expect("read bullfinch pixels");
            assert!(pixels.chunks_exact(4).filter(|p| p[3] > 200).count() > 150, "frame {frame} should be visible");
            assert!(pixels.chunks_exact(4).filter(|p| p[3] == 0).count() > 14000, "background should remain transparent");
            frames.push(pixels);
        }
        assert_ne!(frames[0], frames[1], "flight and perched poses must differ");
        group.sprites[0].pose = [0.0, 1.0, 0.0, 0.0];
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw mirrored bullfinch");
        assert_ne!(frames[0], surface.test_capture.take().unwrap(), "bird must turn toward the cursor");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn fox_texture_renders_with_transparency_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 128, bottom: 128 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 128, 128, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(128.0, 128.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let mut group = SculptureDraw {
            sprites: vec![Sprite { position: V2::new(64.0, 64.0), size: fox::SIZE, alpha: 1.0, mood: [13.0, 0.0], ..Sprite::default() }],
            covers: Vec::new(),
            control: false,
        };
        let mut frames = Vec::new();
        for frame in 0..4 {
            group.sprites[0].pose[0] = frame as f32;
            surface.test_capture = Some(Vec::new());
            surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw fox");
            let pixels = surface.test_capture.take().expect("read rendered pixels");
            let opaque = pixels.chunks_exact(4).filter(|p| p[3] > 200).count();
            let transparent = pixels.chunks_exact(4).filter(|p| p[3] == 0).count();
            assert!(opaque > 500, "frame {frame} should be visible, got {opaque} pixels");
            assert!(transparent > 5000, "background should be transparent, got {transparent} pixels");
            frames.push(pixels);
        }
        assert!(frames.windows(2).all(|pair| pair[0] != pair[1]), "walking frames should differ");
        group.sprites[0].pose = [0.0, 1.0, 0.0, 0.0];
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw fox facing right");
        let right = surface.test_capture.take().expect("read mirrored pixels");
        assert_ne!(frames[0], right, "the fox should turn toward the pointer");

        group.sprites[0].mood[0] = 14.0;
        group.sprites[0].pose = [0.0, 0.0, 34.0, 0.0];
        let mut burrow = Vec::new();
        for frame in 0..4 {
            group.sprites[0].pose[0] = frame as f32;
            surface.test_capture = Some(Vec::new());
            surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw burrow");
            let pixels = surface.test_capture.take().expect("read burrow pixels");
            assert!(pixels.chunks_exact(4).any(|p| p[3] > 200), "burrow frame {frame} should be visible");
            assert!(pixels.chunks_exact(4).enumerate().filter(|(i, _)| i / 128 >= 95).all(|(_, p)| p[3] == 0), "drift must be clipped below snow");
            burrow.push(pixels);
        }
        assert!(burrow.windows(2).all(|pair| pair[0] != pair[1]), "burrow frames should differ");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn fox_mound_uses_live_snow_height_on_gpu() {
        use snowhush_core::simulation::ScatterPiece;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 128, bottom: 128 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 128, 128, (1.0, 1.0)).expect("create drawing surface");
        let mut simulation = SnowSimulation::new(128.0, 128.0, 0.0);
        let pieces: Vec<_> = (0..6000)
            .map(|i| ScatterPiece { position: V2::new(5.0 + (i % 118) as f32, 10.0), velocity: V2::new(0.0, -80.0), color: [1.0; 3] })
            .collect();
        simulation.scatter(&pieces);
        for _ in 0..120 {
            simulation.step(0.05, V2::new(-1000.0, -1000.0));
        }
        assert!(simulation.snow_available(0, 64.0, 15.0) > 0.0, "test must have ground snow");
        // Create a bank with noticeably different heights on its two sides.
        simulation.plow_ground(20.0, 48.0);
        let ground = simulation.ground_surface(64.0);
        let group = SculptureDraw {
            sprites: vec![Sprite {
                position: V2::new(64.0, ground + 30.0),
                size: fox::SIZE,
                alpha: 1.0,
                mood: [14.0, 0.0],
                pose: [0.0, 0.0, ground, 0.0],
                ..Sprite::default()
            }],
            covers: Vec::new(),
            control: false,
        };
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw mound on uneven snow");
        let pixels = surface.test_capture.take().expect("read rendered pixels");
        let mut data = vec![0.0; simulation.column_count()];
        let layers = simulation.write_layers(&mut data);
        let layer = &layers[0];
        let mut contacts = 0;
        for x in 24..104 {
            let f = (x as f32 + 0.5 - layer.origin.x) / snowhush_core::simulation::COLUMN_WIDTH - 0.5;
            let base = f.floor();
            let i = (base as isize).clamp(0, layer.count as isize - 1) as usize;
            let j = (i + 1).min(layer.count - 1);
            let height = layer.origin.y + data[layer.offset + i] + (data[layer.offset + j] - data[layer.offset + i]) * (f - base).clamp(0.0, 1.0);
            for row in 0..128 {
                let y = 128.0 - (row as f32 + 0.5);
                let alpha = pixels[(row * 128 + x) * 4 + 3];
                assert!(alpha == 0 || y + 0.01 >= height, "mound must not draw under live snow");
                if alpha > 20 && y >= height && y < height + 3.0 {
                    contacts += 1;
                }
            }
        }
        assert!(contacts > 15, "mound should meet the changing snow surface, got {contacts} contact pixels");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn sculptures_draw_on_both_overlay_types() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let rect = RECT { left: width - 32, top: 0, right: width, bottom: 32 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 32, 32, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(32.0, 32.0, 0.0);
        let mut snowmen = snowhush_core::snowmen::Snowmen::default();
        assert!(snowmen.restore_with_appearance(V2::new(16.0, 0.0), vec![8.0, 6.0, 5.0], Some((0, 16.0)), 12345));
        let mut group = SculptureDraw {
            sprites: crate::sculpt::sculpture_sprites(&snowmen.stacks()[0], 0.0),
            covers: vec![[0.0, 0.0, 5.0, 5.0]],
            control: false,
        };
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], true).expect("draw with snow");
        group.sprites = crate::sculpt::sculpture_sprites(&snowmen.stacks()[0], 1.0);
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw on control surface");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn snowman_materials_render_pixels_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 200, bottom: 200 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 200, 200, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(200.0, 200.0, 0.0);
        let mut snowmen = snowhush_core::snowmen::Snowmen::default();
        assert!(snowmen.restore_with_appearance(V2::new(100.0, 20.0), vec![30.0, 23.0, 16.0], Some((0, 100.0)), 0x123456789abcdef0));
        let group = SculptureDraw { sprites: crate::sculpt::sculpture_sprites(&snowmen.stacks()[0], 0.0), covers: Vec::new(), control: false };
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw snowman");
        let pixels = surface.test_capture.take().expect("read rendered pixels");
        let orange = pixels.chunks_exact(4).filter(|p| p[2] > 120 && p[2] > p[1].saturating_add(30) && p[1] > p[0]).count();
        let snow = pixels.chunks_exact(4).filter(|p| p[0] > 150 && p[1] > 150 && p[2] > 150).count();
        if let Some(path) = std::env::var_os("SNOWHUSH_TEST_IMAGE") {
            std::fs::write(path, &pixels).expect("write BGRA pixels");
        }
        assert!(orange > 5, "the carrot must be visible, got {orange} pixels");
        assert!(snow > 1000, "the snow must be visible, got {snow} pixels");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn adult_tree_texture_renders_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 200, bottom: 200 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 200, 200, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(200.0, 200.0, 0.0);
        let mut objects = snowhush_core::snowmen::Snowmen::default();
        assert!(objects.restore_with_appearance(V2::new(100.0, 5.0), vec![30.0; 3], Some((0, 100.0)), 1));
        let group = SculptureDraw {
            sprites: crate::sculpt::tree_sprites(&objects.stacks()[0], crate::cone::Species::Spruce, 0, 0.0),
            covers: Vec::new(),
            control: false,
        };
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        surface.test_capture = Some(Vec::new());
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw tree");
        let pixels = surface.test_capture.take().expect("read rendered pixels");
        let green = pixels.chunks_exact(4).filter(|p| p[1] > 50 && p[1] > p[0] && p[1] > p[2]).count();
        let red = pixels.chunks_exact(4).filter(|p| p[2] > 100 && p[2] > p[1].saturating_add(30)).count();
        assert!(green > 500, "tree foliage should be visible, got {green} pixels");
        assert!(red > 10, "tree ornaments should be visible, got {red} pixels");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn cursor_clears_tree_but_not_gift_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 200, bottom: 200 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 200, 200, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(200.0, 200.0, 0.0);
        let mut objects = snowhush_core::snowmen::Snowmen::default();
        assert!(objects.restore_with_appearance(V2::new(100.0, 5.0), vec![30.0; 3], Some((0, 100.0)), 1));
        let tree = SculptureDraw {
            sprites: crate::sculpt::tree_sprites(&objects.stacks()[0], crate::cone::Species::Spruce, 0, 0.0),
            covers: Vec::new(),
            control: false,
        };
        let gift = SculptureDraw {
            sprites: vec![Sprite { position: V2::new(100.0, 70.0), size: 42.0, alpha: 1.0, mood: [15.0, 0.0], ..Sprite::default() }],
            covers: Vec::new(),
            control: false,
        };
        let mut render = |group: &SculptureDraw, cursor| {
            let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (cursor, 0.0) };
            surface.test_capture = Some(Vec::new());
            surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[group], false).expect("draw sprite");
            surface.test_capture.take().unwrap()
        };
        let away = V2::new(-1000.0, -1000.0);
        let cursor = V2::new(100.0, 70.0);
        let tree_away = render(&tree, away);
        let tree_hover = render(&tree, cursor);
        let alpha = |pixels: &[u8]| -> u64 { pixels.chunks_exact(4).map(|p| u64::from(p[3])).sum() };
        assert!(alpha(&tree_hover) < alpha(&tree_away) * 8 / 10, "cursor should clear the tree locally");
        assert_eq!(render(&gift, away), render(&gift, cursor), "the gift must keep its opacity");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn collectible_animation_frames_render_on_gpu() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 512, bottom: 256 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 512, 256, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(512.0, 256.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mut groups = Vec::new();
        for (index, age) in [0.05_f32, 0.14, 0.27, 0.5].into_iter().enumerate() {
            let x = 60.0 + index as f32 * 95.0;
            let t = (age / 0.35).clamp(0.0, 1.0);
            let mut cone = cone_sprites(V2::new(x, 200.0), false, crate::cone::Species::Spruce);
            for sprite in &mut cone {
                sprite.size *= (1.0 - t).powi(2).max(0.01);
                sprite.alpha *= 1.0 - t;
            }
            let mut trees = snowhush_core::snowmen::Snowmen::default();
            let base = V2::new(x, 115.0);
            assert!(trees.restore_with_appearance(base, vec![30.0; 3], Some((0, x)), now << 32));
            let mut sprout = crate::sculpt::tree_sprites(&trees.stacks()[0], crate::cone::Species::Spruce, 0, 0.0);
            let scale = crate::sculpt::birth_scale(age);
            for sprite in &mut sprout {
                sprite.position = base + (sprite.position - base) * scale;
                sprite.size *= scale.max(0.01);
                sprite.alpha *= (age / 0.55 * 6.0).min(1.0);
            }
            cone.extend(sprout);
            groups.push(SculptureDraw { sprites: cone, covers: Vec::new(), control: false });
        }
        for (index, seconds) in [90_u64, 200].into_iter().enumerate() {
            let x = 380.0 + index as f32 * 90.0;
            let mut trees = snowhush_core::snowmen::Snowmen::default();
            assert!(trees.restore_with_appearance(V2::new(x, 4.0), vec![18.0; 3], Some((0, x)), (now - seconds) << 32));
            groups.push(SculptureDraw {
                sprites: crate::sculpt::tree_sprites(&trees.stacks()[0], crate::cone::Species::Spruce, 0, 0.0),
                covers: Vec::new(),
                control: false,
            });
        }
        surface.test_capture = Some(Vec::new());
        let refs: Vec<_> = groups.iter().collect();
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &refs, false).expect("draw animation frames");
        let pixels = surface.test_capture.take().expect("read rendered pixels");
        assert!(pixels.chunks_exact(4).filter(|p| p[3] > 30).count() > 500);
        if let Some(path) = std::env::var_os("SNOWHUSH_ANIMATION_IMAGE") {
            std::fs::write(path, pixels).unwrap();
        }
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn all_conifer_species_render_together_on_gpu() {
        use crate::cone::Species;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 480, bottom: 320 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 480, 320, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(480.0, 320.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mut groups = Vec::new();
        for (index, species) in [Species::Spruce, Species::Pine, Species::Fir].into_iter().enumerate() {
            let x = 80.0 + index as f32 * 160.0;
            groups.push(SculptureDraw { sprites: cone_sprites(V2::new(x, 291.0), false, species), covers: Vec::new(), control: false });
            for (age, y) in [(0, 240.0), (90, 170.0), (200, 5.0)] {
                let mut trees = snowhush_core::snowmen::Snowmen::default();
                assert!(trees.restore_with_appearance(V2::new(x, y), vec![25.0; 3], Some((0, x)), (now - age) << 32 | index as u64));
                groups.push(SculptureDraw {
                    sprites: crate::sculpt::tree_sprites(&trees.stacks()[0], species, 0, 0.0),
                    covers: Vec::new(),
                    control: false,
                });
            }
        }
        surface.test_capture = Some(Vec::new());
        let refs: Vec<_> = groups.iter().collect();
        surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &refs, false).expect("draw all species");
        let pixels = surface.test_capture.take().expect("read rendered pixels");
        for column in 0..3 {
            let visible = pixels.chunks_exact(4).enumerate().filter(|(index, p)| index % 480 / 160 == column && p[3] > 30).count();
            assert!(visible > 500, "species {column} must render, got {visible} pixels");
        }
        if let Some(path) = std::env::var_os("SNOWHUSH_CONIFERS_IMAGE") {
            std::fs::write(path, pixels).unwrap();
        }
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn conifer_variants_and_animated_garlands_render_on_gpu() {
        use crate::cone::Species;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 510, bottom: 380 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 510, 380, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(510.0, 380.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let render = |surface: &mut Surface, time: f64| {
            let mut groups = Vec::new();
            for (index, species) in [Species::Spruce, Species::Pine, Species::Fir].into_iter().enumerate() {
                let x = 85.0 + index as f32 * 170.0;
                groups.push(SculptureDraw { sprites: cone_sprites(V2::new(x, 350.0), false, species), covers: Vec::new(), control: false });
                for (variant, y) in [(0, 180.0), (1, 5.0)] {
                    let mut trees = snowhush_core::snowmen::Snowmen::default();
                    assert!(trees.restore_with_appearance(V2::new(x, y), vec![25.0; 3], Some((0, x)), (now - 200) << 32 | index as u64));
                    groups.push(SculptureDraw {
                        sprites: crate::sculpt::tree_sprites(&trees.stacks()[0], species, variant, time),
                        covers: Vec::new(),
                        control: false,
                    });
                }
            }
            surface.test_capture = Some(Vec::new());
            let refs: Vec<_> = groups.iter().collect();
            surface.draw(&gpu, &simulation, ambient, time as f32, None, None, None, &refs, false).expect("draw trees and garlands");
            surface.test_capture.take().expect("read rendered pixels")
        };
        let first = render(&mut surface, 0.0);
        let later = render(&mut surface, 1.0);
        let changed = first.chunks_exact(4).zip(later.chunks_exact(4)).filter(|(a, b)| a != b).count();
        assert!(changed > 50, "garland animation must change rendered pixels, got {changed}");
        if let Some(path) = std::env::var_os("SNOWHUSH_VARIANTS_IMAGE") {
            std::fs::write(path, first).unwrap();
        }
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn ornaments_shimmer_without_the_garland_overlay() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 200, bottom: 200 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 200, 200, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(200.0, 200.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let mut objects = snowhush_core::snowmen::Snowmen::default();
        assert!(objects.restore_with_appearance(V2::new(100.0, 5.0), vec![30.0; 3], Some((0, 100.0)), 7));
        let render = |surface: &mut Surface, time| {
            let tree = crate::sculpt::tree_sprites(&objects.stacks()[0], crate::cone::Species::Spruce, 0, time)[0];
            let group = SculptureDraw { sprites: vec![tree], covers: Vec::new(), control: false };
            surface.test_capture = Some(Vec::new());
            surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw ornament glints");
            surface.test_capture.take().unwrap()
        };
        let first = render(&mut surface, 0.0);
        let later = render(&mut surface, 2.0);
        let changed = first.chunks_exact(4).zip(later.chunks_exact(4)).filter(|(a, b)| a != b).count();
        assert!(changed > 20, "ornaments should shimmer even without garland sprites, got {changed}");
    }

    #[test]
    #[ignore = "requires an interactive desktop and Direct3D graphics adapter"]
    fn pixel_trees_do_not_ripple_when_snow_moves_less_than_a_pixel() {
        use crate::cone::Species;
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let gpu = Gpu::new().expect("create Direct3D device");
        let rect = RECT { left: 0, top: 0, right: 200, bottom: 200 };
        let overlay = Overlay(create_overlay(&rect, true).expect("create overlay"));
        let mut surface = Surface::new(&gpu, overlay.0, 200, 200, (1.0, 1.0)).expect("create drawing surface");
        let simulation = SnowSimulation::new(200.0, 200.0, 0.0);
        let ambient = Ambient { light: [1.0; 3], shadow_tint: [0.0; 4], rim: [0.0; 4], lantern: (V2::ZERO, 0.0) };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let render = |surface: &mut Surface, species, age, variant, offset: f32| {
            let mut trees = snowhush_core::snowmen::Snowmen::default();
            let base = V2::new(100.0 + offset, 15.0 + offset);
            assert!(trees.restore_with_appearance(base, vec![28.37; 3], Some((0, base.x)), (now - age) << 32));
            let tree = crate::sculpt::tree_sprites(&trees.stacks()[0], species, variant, 0.0)[0];
            let group = SculptureDraw { sprites: vec![tree], covers: Vec::new(), control: false };
            surface.test_capture = Some(Vec::new());
            surface.draw(&gpu, &simulation, ambient, 0.0, None, None, None, &[&group], false).expect("draw tree");
            surface.test_capture.take().unwrap()
        };
        for species in [Species::Spruce, Species::Pine, Species::Fir] {
            for (age, variant) in [(0, 0), (90, 0), (200, 0), (200, 1)] {
                let first = render(&mut surface, species, age, variant, 0.05);
                let second = render(&mut surface, species, age, variant, 0.45);
                let changed = first.chunks_exact(4).zip(second.chunks_exact(4)).filter(|(a, b)| a != b).count();
                assert_eq!(changed, 0, "{species:?} at age {age}, variant {variant}: {changed} pixels shifted within the same screen-pixel position");
                let moved = render(&mut surface, species, age, variant, 1.05);
                let mut mismatch = 0;
                for y in 0..199_usize {
                    for x in 1..200_usize {
                        let old = ((y + 1) * 200 + x - 1) * 4;
                        let new = (y * 200 + x) * 4;
                        mismatch += usize::from(first[old..old + 4] != moved[new..new + 4]);
                    }
                }
                assert_eq!(
                    mismatch, 0,
                    "{species:?} at age {age}, variant {variant}: {mismatch} pixels changed when the sprite moved by one whole pixel"
                );
            }
        }
    }
}
