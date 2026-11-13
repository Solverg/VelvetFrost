use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::sync::mpsc::Receiver;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, CreateWaitableTimerW, INFINITE, SetWaitableTimer, TIMER_ALL_ACCESS,
    WaitForMultipleObjects,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MB_ICONERROR, MB_OK, MSG, MWMO_INPUTAVAILABLE, MessageBoxW, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, PostMessageW,
    QS_ALLINPUT, TranslateMessage, WM_CLOSE,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

use snowhush_core::geometry::V2;
use snowhush_core::halloween::Choice;
use snowhush_core::simulation::{Mode, ScatterPiece, SnowSimulation};

use crate::desktop::{self, DesktopLayer};
use crate::halloween;
use crate::light::{Sky, Wallpapers};
use crate::log;
use crate::plow::Plow;
use crate::prefs::{self, Settings};
use crate::render::Gpu;
use crate::screen::{BACKDROP_CHANGED, Clock, DISPLAYS_CHANGED, Monitor, PowerBudget, Screen, dip_size, listen, monitors, walls, watch_taskbar};
use crate::sleigh::Sleigh;
use crate::tracker::{self, Tracker};
use crate::traffic::{self, Traffic, Vehicle};
use crate::truck::Truck;

#[derive(Clone, PartialEq, Eq)]
pub struct CollectionEntry {
    pub monitor: String,
    pub object: CollectionObject,
    pub stored: bool,
    /// Current displayed tree height in virtual millimetres, or None for other objects.
    pub tree_height_mm: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CollectionObject {
    Tree(u64, crate::cone::Species, u8),
    Gift(crate::gifts::GiftKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionError {
    WrongDisplay,
    NoRoom,
    Busy,
    Unavailable,
    SaveFailed,
    Unconfirmed,
}

/// A piece of the welcome window crumbling into snow, in physical screen pixels.
pub struct Piece {
    pub x: f32,
    pub y: f32,
    pub velocity: V2,
    pub color: [f32; 3],
}

pub enum Command {
    CollectionSnapshot(std::sync::mpsc::Sender<Vec<CollectionEntry>>),
    CollectionMove(CollectionEntry, std::sync::mpsc::Sender<std::result::Result<usize, CollectionError>>),
    CollectionUndo(String, std::sync::mpsc::Sender<std::result::Result<(), CollectionError>>),
    CollectionPlace(CollectionEntry, i32, i32, std::sync::mpsc::Sender<std::result::Result<(), CollectionError>>),
    /// How much falls, 0 to 1.
    Intensity(f32),
    /// How opaque what falls is drawn, 0.1 to 1.
    FallingOpacity(f32),
    Accumulate(bool),
    Melt(bool),
    SnowOnWindows(bool),
    BehindWindows(bool),
    TreesOnTop(bool),
    TreesBeforeSnowmen(bool),
    OverFullScreen(bool),
    /// Every frame at the display's highest rate, or as fast as what moves needs.
    Performance(bool),
    BehindTaskbar(bool),
    /// The enabled displays changed in the tray menu.
    DisplaysChanged,
    /// Explorer started again, taking the desktop's wallpaper layer with it.
    ShellRestarted,
    ShowInScreenshots(bool),
    /// The snow settles on a selection dragged across the desktop too.
    DeepInteraction(bool),
    /// Off while the screen is locked, asleep or the session is away.
    Active(bool),
    Scatter(Vec<Piece>),
    /// Starts the snowfall held back for the welcome window.
    ReleaseSnowfall,
    /// Blows all the snow away; when calm, for less motion, melts it where it lies instead.
    ClearSnow(bool),
    /// Set the interval; a nonzero selection starts a pass immediately.
    PlowInterval(u32),
    TruckEnabled(bool),
    SleighEnabled(bool),
    /// Halloween turned on or off from the menu.
    Halloween(Choice),
    /// Other kinds fall, once a gust has blown what is there away; when calm, for less motion, it melts where it
    /// lies instead. A kind only joining what falls starts at once.
    SwitchMode {
        mode: Mode,
        mixed: bool,
        mix: [Mode; 2],
        calm: bool,
    },
    Quit,
}

/// Raw handles cross to the snow thread as numbers: the thread only waits on the event and posts to the window.
pub struct Links {
    pub wake: isize,
    pub controller: isize,
}

pub fn run(commands: Receiver<Command>, links: Links, settings: Settings) {
    if let Err(error) = run_loop(&commands, &links, settings) {
        log!("stopped: {error}");
        let text = HSTRING::from(format!("VelvetFrost couldn't start drawing snow.\n\n{error}"));
        unsafe {
            MessageBoxW(None, &text, w!("VelvetFrost"), MB_OK | MB_ICONERROR);
            let _ = PostMessageW(Some(HWND(links.controller as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn run_loop(commands: &Receiver<Command>, links: &Links, mut settings: Settings) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let mut gpu = Gpu::new()?;
    log!("adapter: {}", gpu.adapter);
    let _listener = listen()?;
    let mut layer = settings.behind_windows.then(find_layer).flatten();
    // The displays whose taskbar a full-screen app the snow shows over has put away, and since when each display has
    // been seen that way or not.
    let mut full_screen: HashSet<String> = HashSet::new();
    let mut full_screen_seen: HashMap<String, (bool, i64)> = HashMap::new();
    let mut screens = sync_screens(&gpu, &settings, &full_screen, Vec::new(), layer, false);
    let timer = unsafe {
        CreateWaitableTimerExW(None, PCWSTR::null(), CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)
            .or_else(|_| CreateWaitableTimerW(None, false, PCWSTR::null()))?
    };
    let wake = HANDLE(links.wake as _);
    let mut clock = Clock::new();
    let started = Clock::now();
    let mut traffic = Traffic::new(settings.plow_interval, 0, started, clock.frequency);
    let directions = std::collections::hash_map::RandomState::new();
    let mut events = crate::events::Schedule::new(started, clock.frequency);
    let mut active = true;
    let mut budget = PowerBudget::Full;
    let mut spooky = halloween::shows(&settings);
    let mut sky = Sky::now(spooky);
    let wallpapers = Wallpapers::new().inspect_err(|error| log!("wallpapers unavailable: {error}")).ok();
    let mut chores_at = 0;
    let mut light_at = 0;
    let mut displays_changed_at = None;
    let mut desktop_logged = String::new();
    let mut stats = Stats::new(started);
    let mut tracker = Tracker::new();
    watch_taskbar(links.wake);

    loop {
        screens.iter().filter(|screen| screen.under_taskbar()).for_each(Screen::raise);
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::CollectionSnapshot(reply) => {
                    let _ = reply.send(screens.iter().flat_map(Screen::collection_entries).collect());
                }
                Command::CollectionMove(entry, reply) => {
                    if let Some(screen) = screens.iter_mut().find(|screen| screen.monitor.name == entry.monitor) {
                        let result = screen.move_collection_item(entry.object, !entry.stored);
                        screen.wake(Clock::now());
                        let _ = reply.send(result);
                    } else {
                        let _ = reply.send(Err(CollectionError::Unavailable));
                    }
                }
                Command::CollectionUndo(display, reply) => {
                    let result =
                        screens.iter_mut().find(|screen| screen.monitor.name == display).ok_or(CollectionError::Unavailable).and_then(|screen| {
                            let result = screen.undo_collection_move();
                            screen.wake(Clock::now());
                            result
                        });
                    let _ = reply.send(result);
                }
                Command::CollectionPlace(entry, x, y, reply) => {
                    let placed = if entry.stored {
                        if let Some(screen) = screens.iter_mut().find(|screen| screen.monitor.name == entry.monitor) {
                            if !screen.contains(x as f32, y as f32) {
                                let _ = reply.send(Err(CollectionError::WrongDisplay));
                                continue;
                            }
                            let at = screen.local(x as f32, y as f32);
                            let placed = screen.place_collection_item(entry.object, at);
                            if placed.is_ok() {
                                screen.wake(Clock::now());
                            }
                            placed
                        } else {
                            Err(CollectionError::Unavailable)
                        }
                    } else {
                        Err(CollectionError::Unavailable)
                    };
                    let _ = reply.send(placed);
                }
                Command::Quit => return Ok(()),
                Command::Intensity(intensity) => {
                    settings.intensity = (intensity * 100.0).round() as u32;
                    let now = Clock::now();
                    screens.iter_mut().for_each(|screen| {
                        screen.simulation.set_intensity(intensity);
                        screen.wake(now);
                    });
                }
                Command::FallingOpacity(opacity) => {
                    settings.opacity = (opacity * 100.0).round() as u32;
                    screens.iter_mut().for_each(|screen| screen.simulation.falling_opacity = opacity);
                }
                Command::Accumulate(on) => {
                    settings.accumulate = on;
                    screens.iter_mut().for_each(|screen| screen.simulation.accumulates = on);
                }
                Command::Melt(on) => {
                    settings.melt = on;
                    screens.iter_mut().for_each(|screen| screen.simulation.thaws = on);
                }
                Command::SnowOnWindows(on) => {
                    settings.snow_on_windows = on;
                    screens.iter_mut().for_each(|screen| screen.set_snow_on_windows(on));
                }
                Command::BehindWindows(on) => {
                    settings.behind_windows = on;
                    if on {
                        settings.trees_on_top = false;
                    }
                    layer = on.then(find_layer).flatten();
                    screens = sync_screens(&gpu, &settings, &full_screen, screens, layer, true);
                }
                Command::TreesOnTop(on) => {
                    let on = prefs::valid_tree_front(settings.behind_windows, on);
                    settings.trees_on_top = on;
                    let now = Clock::now();
                    screens.iter_mut().for_each(|screen| screen.set_trees_on_top(&gpu, on, now));
                }
                Command::TreesBeforeSnowmen(on) => {
                    settings.trees_before_snowmen = on;
                    let now = Clock::now();
                    screens.iter_mut().for_each(|screen| screen.set_trees_before_snowmen(on, now));
                }
                Command::OverFullScreen(on) => {
                    settings.over_full_screen = on;
                    if !on && !full_screen.is_empty() {
                        full_screen.clear();
                        screens = sync_screens(&gpu, &settings, &full_screen, screens, layer, false);
                    }
                }
                Command::DeepInteraction(on) => {
                    settings.deep_interaction = on;
                    if !on {
                        tracker.drop_marquee();
                    }
                }
                Command::Performance(max) => {
                    settings.max_performance = max;
                    screens.iter_mut().for_each(|screen| screen.max_rate = max);
                }
                Command::BehindTaskbar(on) => {
                    settings.behind_taskbar = on;
                    screens = sync_screens(&gpu, &settings, &full_screen, screens, layer, true);
                }
                Command::DisplaysChanged => {
                    screens = sync_screens(&gpu, &settings, &full_screen, screens, layer, false);
                    light_at = 0;
                }
                Command::ShellRestarted => {
                    if settings.behind_windows {
                        layer = find_layer();
                        screens = sync_screens(&gpu, &settings, &full_screen, screens, layer, true);
                    }
                    let now = Clock::now();
                    screens.iter_mut().for_each(|screen| screen.set_trees_on_top(&gpu, settings.trees_on_top, now));
                }
                Command::ShowInScreenshots(on) => {
                    settings.show_in_screenshots = on;
                    screens.iter_mut().for_each(|screen| screen.show_in_screenshots(on));
                }
                Command::Scatter(pieces) => {
                    let Some(first) = pieces.first() else { continue };
                    let index = screens.iter().position(|screen| screen.contains(first.x, first.y)).unwrap_or(0);
                    let Some(screen) = screens.get_mut(index) else { continue };
                    let local: Vec<ScatterPiece> = pieces
                        .iter()
                        .map(|piece| ScatterPiece { position: screen.local(piece.x, piece.y), velocity: piece.velocity, color: piece.color })
                        .collect();
                    screen.simulation.scatter(&local);
                    screen.wake(Clock::now());
                }
                Command::ReleaseSnowfall => {
                    settings.welcomed = true;
                    screens.iter_mut().for_each(|screen| screen.simulation.set_snowfall_held(false));
                }
                Command::ClearSnow(calm) => {
                    let direction = screens.first().map_or(-1.0, |screen| screen.simulation.breeze_direction());
                    screens.iter_mut().for_each(|screen| screen.simulation.blow_away(direction, calm));
                }
                Command::PlowInterval(minutes) => {
                    settings.plow_interval = minutes;
                    let now = Clock::now();
                    traffic.select(Vehicle::Plow, minutes, now);
                    if minutes == 0 {
                        screens.iter_mut().for_each(|screen| screen.stop_plow(now));
                    }
                }
                Command::TruckEnabled(enabled) => {
                    settings.truck_enabled = enabled;
                    let now = Clock::now();
                    if !enabled {
                        screens.iter_mut().for_each(|screen| screen.stop_truck(now));
                    }
                }
                Command::SleighEnabled(enabled) => {
                    settings.sleigh_enabled = enabled;
                    let now = Clock::now();
                    if !enabled {
                        screens.iter_mut().for_each(|screen| screen.stop_sleigh(now));
                    }
                }
                Command::Halloween(choice) => {
                    settings.halloween = choice;
                    chores_at = 0;
                    light_at = 0;
                }
                Command::SwitchMode { mode, mixed, mix, calm } => {
                    settings.mode = mode;
                    settings.mixed = mixed;
                    settings.mix = mix;
                    light_at = 0;
                    let modes = settings.modes();
                    let direction = screens.first().map_or(-1.0, |screen| screen.simulation.breeze_direction());
                    let now = Clock::now();
                    screens.iter_mut().for_each(|screen| {
                        if modes != [Mode::Snow] {
                            screen.stop_plow(now);
                            screen.stop_truck(now);
                        }
                        screen.simulation.switch_modes(&modes, direction, calm);
                        screen.wake(now);
                    });
                }
                Command::Active(on) => {
                    if on && !active {
                        screens.iter_mut().for_each(Screen::resume);
                    } else if !on && active {
                        screens.iter_mut().for_each(Screen::stop_sculpting);
                    }
                    tracker::watch_shell_state(on);
                    active = on;
                    log!("active: {on}");
                }
            }
        }

        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        let mut now = Clock::now();
        if DISPLAYS_CHANGED.take() {
            displays_changed_at = Some(now);
        }
        // Display changes come in bursts; the layout is read once they settle.
        if let Some(changed_at) = displays_changed_at
            && clock.seconds(now - changed_at) > 0.4
        {
            displays_changed_at = None;
            if settings.behind_windows {
                layer = find_layer();
            }
            screens = sync_screens(&gpu, &settings, &full_screen, screens, layer, false);
            clock.refresh();
            light_at = 0;
        }
        if BACKDROP_CHANGED.take() {
            light_at = 0;
        }

        if now >= chores_at {
            chores_at = now + clock.frequency;
            clock.refresh();
            // In case a display change went unannounced.
            let current: Vec<_> = monitors(over_taskbar(&settings, &screens), &full_screen)
                .into_iter()
                .filter(|monitor| prefs::display_enabled(&monitor.name))
                .collect();
            if current.len() != screens.len() || screens.iter().any(|screen| !current.contains(&screen.monitor)) {
                DISPLAYS_CHANGED.set(true);
            }
            let next = power_budget();
            if next != budget {
                budget = next;
                log!("power: {budget:?}");
                let density = if budget == PowerBudget::Saver { 0.75 } else { 1.0 };
                screens.iter_mut().for_each(|screen| screen.simulation.set_density(density));
            }
            // Behind windows, a SetWindowPos every second on every monitor's child overlay restacks them under the
            // icons and the desktop layer blinks — often on one display, then another. Raise only when the
            // wallpaper layer's children change (a live wallpaper landing on top, Explorer rebuilding, …).
            if settings.behind_windows && !settings.trees_on_top {
                let tree = desktop::tree();
                if tree != desktop_logged {
                    log!("desktop: {tree}");
                    desktop_logged = tree;
                    screens.iter().for_each(Screen::raise);
                }
            } else {
                screens.iter().for_each(Screen::raise);
            }
            // Behind windows, the overlays live inside Explorer's windows and vanish when it restarts; until the
            // wallpaper layer is found they wait on top of everything instead.
            if settings.behind_windows && (layer.is_none() || screens.iter().any(|screen| !screen.alive())) {
                let found = find_layer();
                if found.is_some() || screens.iter().any(|screen| !screen.alive()) {
                    layer = found;
                    screens = sync_screens(&gpu, &settings, &full_screen, screens, layer, true);
                }
            }
            for screen in &mut screens {
                if screen.tree_overlay_missing() {
                    screen.set_trees_on_top(&gpu, settings.trees_on_top, now);
                }
            }
            if now >= light_at {
                light_at = now + clock.frequency * 60;
                let on = halloween::shows(&settings);
                if on != spooky {
                    spooky = on;
                    log!("halloween: {on}");
                    screens.iter_mut().for_each(|screen| {
                        screen.simulation.set_halloween(on);
                        screen.wake(now);
                    });
                }
                sky = Sky::now(spooky);
                if let Some(wallpapers) = &wallpapers {
                    screens.iter_mut().for_each(|screen| screen.update_backdrop(wallpapers));
                }
            }
        }

        if active {
            // A drag starting is drawn right away instead of at the next calm frame.
            if tracker.take_started_moving() && settings.snow_on_windows {
                screens.iter_mut().for_each(|screen| screen.wake(now));
            }
            // The desktop's rubber band moves with the pointer, so it is drawn right away too.
            if settings.snow_on_windows && settings.deep_interaction && tracker.follow_marquee() {
                screens.iter_mut().for_each(|screen| screen.wake(now));
            }
            if screens.iter().any(|screen| screen.next_due <= now + clock.tolerance()) {
                if tracker.refresh(&clock, now) && tracker.animating() {
                    screens.iter_mut().for_each(|screen| screen.wake(now));
                }
            }
            let vehicle_busy = screens.iter().any(|screen| screen.plow.is_some() || screen.truck.is_some());
            // sync_screens contains only displays enabled in Settings › Monitors.
            let eligible = |screen: &Screen| {
                screen.simulation.modes() == [Mode::Snow] && !screen.covered() && tracker.covering(&screen.monitor.bounds).is_none()
            };
            let vehicle_allowed = !vehicle_busy && settings.modes() == [Mode::Snow] && screens.iter().any(eligible);
            if let Some(vehicle) = traffic.poll(now, clock.frequency, vehicle_allowed, vehicle_busy) {
                let choices: Vec<bool> = screens.iter().map(eligible).collect();
                if let Some(index) = traffic::choose_monitor(&choices, directions.hash_one((now, "monitor"))) {
                    start_vehicle(&mut screens[index], now, &directions, vehicle, false);
                }
            }
            if events.due(now) {
                use crate::events::Event;
                let snow_enabled = settings.intensity > 0 && settings.modes().contains(&Mode::Snow);
                let visible = |screen: &Screen| {
                    snow_enabled
                        && screen.simulation.modes().contains(&Mode::Snow)
                        && !screen.covered()
                        && tracker.covering(&screen.monitor.bounds).is_none()
                };
                let choices: Vec<bool> = screens.iter().map(visible).collect();
                let cone_monitor = traffic::choose_monitor(&choices, directions.hash_one((now, "cone monitor")));
                let cone_random = directions.hash_one((now, "cone appearance"));
                let species = crate::cone::Species::from_random(cone_random);
                let counts = cone_monitor.map_or([0; 3], |index| screens[index].tree_population());
                let gift_choices: Vec<bool> = screens.iter().map(|screen| visible(screen) && screen.can_spawn_gift()).collect();
                let gift_monitor = traffic::choose_monitor(&gift_choices, directions.hash_one((now, "gift monitor")));
                let gifts = gift_monitor.map_or(0, |index| screens[index].gift_count());
                // Keep each design reserved when its display is disconnected; saved and stored trees count too.
                let available: Option<Vec<u8>> = crate::star_event::LEGENDARY_VARIANTS.iter().try_fold(Vec::new(), |mut available, &variant| {
                    let owner = prefs::sculptures(crate::star_event::owner_key(variant)).ok()?;
                    let displays: Vec<_> = screens.iter().map(|screen| (screen.monitor.name.as_str(), screen.has_legendary_tree(variant))).collect();
                    let occupied = displays.iter().any(|(_, present)| *present);
                    let state = crate::star_event::owner_state(owner.as_deref(), &displays);
                    let free = match state {
                        crate::star_event::OwnerState::Available => true,
                        crate::star_event::OwnerState::Occupied => false,
                        crate::star_event::OwnerState::Stale => prefs::set_sculptures(crate::star_event::owner_key(variant), ""),
                    };
                    if free && !occupied {
                        available.push(variant);
                    }
                    Some(available)
                });
                let owned = available.as_ref().map_or(0, |available| crate::star_event::LEGENDARY_VARIANTS.len() - available.len());
                let event =
                    events.poll(now, directions.hash_one((now, "event lottery")), counts.iter().sum(), counts[species.code() as usize], gifts, owned);
                match event {
                    Some(Event::Truck | Event::RoadTrain) if settings.truck_enabled => {
                        let choices: Vec<bool> = screens.iter().map(eligible).collect();
                        let selected = traffic::choose_monitor(&choices, directions.hash_one((now, "truck monitor")));
                        if let Some(index) = selected
                            && traffic.start_unscheduled(Vehicle::Truck, now, vehicle_allowed, vehicle_busy)
                        {
                            start_vehicle(&mut screens[index], now, &directions, Vehicle::Truck, event == Some(Event::RoadTrain));
                        }
                    }
                    Some(Event::Cone) if !screens.iter().any(|screen| screen.has_cone() || screen.has_star_chain()) => {
                        if let Some(index) = cone_monitor {
                            screens[index].spawn_cone(cone_random, now);
                        }
                    }
                    Some(Event::Fox)
                        if !screens.iter().any(|screen| {
                            screen.has_fox() || screen.has_bullfinch() || screen.plow.is_some() || screen.truck.is_some() || screen.sleigh.is_some()
                        }) =>
                    {
                        if let Some(index) = traffic::choose_monitor(&choices, directions.hash_one((now, "fox monitor"))) {
                            screens[index].spawn_fox(directions.hash_one((now, "fox position", index)), now);
                        }
                    }
                    Some(Event::Bullfinch)
                        if !screens.iter().any(|screen| {
                            screen.has_fox() || screen.has_bullfinch() || screen.plow.is_some() || screen.truck.is_some() || screen.sleigh.is_some()
                        }) =>
                    {
                        if let Some(index) = traffic::choose_monitor(&choices, directions.hash_one((now, "bullfinch monitor"))) {
                            screens[index].spawn_bullfinch(directions.hash_one((now, "bullfinch position", index)), now);
                        }
                    }
                    Some(Event::Gift) => {
                        if let Some(index) = gift_monitor {
                            screens[index].spawn_gift(directions.hash_one((now, "gift tree")), directions.hash_one((now, "gift color")), now);
                        }
                    }
                    Some(Event::Star) => {
                        let reward = available
                            .as_ref()
                            .and_then(|available| crate::star_event::choose_reward(available, directions.hash_one((now, "legendary reward"))));
                        if let Some(reward) = reward
                            && !screens.iter().any(|screen| screen.has_star_chain() || screen.has_cone())
                        {
                            let choices: Vec<bool> = screens.iter().map(|screen| visible(screen) && screen.can_start_star_chain()).collect();
                            if let Some(index) = traffic::choose_monitor(&choices, directions.hash_one((now, "star monitor"))) {
                                screens[index].start_star_chain(directions.hash_one((now, "star chain")), owned, reward, now);
                            }
                        }
                    }
                    Some(Event::Sleigh) => {
                        let flying = screens.iter().any(|screen| screen.sleigh.is_some());
                        let choices: Vec<bool> = screens
                            .iter()
                            .map(|screen| {
                                crate::sleigh::monitor_eligible(
                                    screen.simulation.modes() == [Mode::Snow] && settings.modes() == [Mode::Snow],
                                    screen.covered(),
                                    tracker.covering(&screen.monitor.bounds).is_some(),
                                )
                            })
                            .collect();
                        let selected = traffic::choose_monitor(&choices, directions.hash_one((now, "sleigh monitor")));
                        if crate::sleigh::can_start(settings.sleigh_enabled, flying)
                            && let Some(index) = selected
                        {
                            let screen = &mut screens[index];
                            let bounds = &screen.monitor.bounds;
                            let random = directions.hash_one((now, "flight", &screen.monitor.name));
                            screen.sleigh = Some(Sleigh::new(
                                screen.simulation.width(),
                                screen.simulation.height(),
                                bounds.right - bounds.left,
                                bounds.bottom - bounds.top,
                                random,
                            ));
                            screen.wake(now);
                        }
                    }
                    Some(Event::Ghost) if spooky => {
                        let ghost_choices: Vec<bool> =
                            screens.iter().map(|screen| !screen.covered() && tracker.covering(&screen.monitor.bounds).is_none()).collect();
                        if let Some(index) = traffic::choose_monitor(&ghost_choices, directions.hash_one((now, "ghost monitor"))) {
                            if screens[index].simulation.spawn_ghost() {
                                screens[index].wake(now);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Behind the windows the snow still settles on them: what lies on a window's top edge is above it, in plain view.
            let time = (clock.seconds(now - started) % 1000.0) as f32;
            let mut lost = false;
            let mut released = false;
            let mut reshaped = false;
            for screen in screens.iter_mut().filter(|screen| screen.next_due <= now + clock.tolerance()) {
                let in_front = tracker.covering(&screen.monitor.bounds);
                // Shown over a full-screen app, the snow reaches down to the bottom of the display while the app has put
                // the taskbar away, so the drift lies along the bottom instead of over a taskbar no longer there; a
                // change counts once it has lasted, as the app goes full screen or comes out of it in a few steps.
                if settings.over_full_screen && !settings.behind_windows {
                    let wanted = in_front.is_some();
                    match full_screen_seen.get(&screen.monitor.name) {
                        Some(&(state, _)) if state == wanted => {}
                        _ => {
                            full_screen_seen.insert(screen.monitor.name.clone(), (wanted, now));
                        }
                    }
                    if let Some(&(state, since)) = full_screen_seen.get(&screen.monitor.name)
                        && state != full_screen.contains(&screen.monitor.name)
                        && clock.seconds(now - since) > 0.3
                    {
                        log!("{} {}", screen.monitor.name, if state { "taskbar put away by a full-screen app" } else { "taskbar back" });
                        if state {
                            full_screen.insert(screen.monitor.name.clone());
                        } else {
                            full_screen.remove(&screen.monitor.name);
                        }
                        reshaped = true;
                    }
                }
                let covering = if settings.over_full_screen { None } else { in_front };
                if covering.is_some() != screen.covered() {
                    log!("{} {}", screen.monitor.name, covering.map_or("uncovered".to_string(), |class| format!("covered by {class}")));
                    screen.set_covered(&gpu, covering.is_some());
                }
                if in_front.is_some() {
                    screen.stop_plow(now);
                    screen.stop_truck(now);
                    screen.stop_sleigh(now);
                }
                if screen.hidden() {
                    screen.next_due = now + clock.frequency / 4;
                    continue;
                }
                let began = Clock::now();
                match screen.frame(&gpu, &clock, now, budget, sky, time, &tracker, settings.snow_on_windows) {
                    Ok(drew) => {
                        stats.frame(drew, Clock::now() - began, screen.simulation.sprite_count());
                        released |= screen.simulation.take_snowfall_released();
                    }
                    Err(error) => {
                        log!("frame failed: {error}");
                        lost = true;
                    }
                }
            }
            // The first display the welcome window's snow reaches starts the snowfall on all of them.
            if released {
                settings.welcomed = true;
                screens.iter_mut().for_each(|screen| screen.simulation.set_snowfall_held(false));
            }
            if lost {
                screens.clear();
                gpu = Gpu::new()?;
                screens = sync_screens(&gpu, &settings, &full_screen, Vec::new(), layer, false);
            } else if reshaped {
                screens = sync_screens(&gpu, &settings, &full_screen, screens, layer, false);
            }
            now = Clock::now();
            stats.report(&clock, now, &mut tracker);
        }

        let mut cursor = windows::Win32::Foundation::POINT::default();
        let hovering = active
            && screens.iter().any(Screen::collection_dragging)
            && unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor) }.is_ok()
            && crate::inventory::drop_target_at(cursor);
        crate::inventory::set_drop_hover(hovering);
        let due = screens.iter().map(|screen| screen.next_due).min().filter(|_| active);
        unsafe {
            if let Some(due) = due {
                let wait = -(((due - now).max(0) as i128 * 10_000_000) / clock.frequency as i128) as i64;
                let _ = SetWaitableTimer(timer, &wait, 0, None, None, false);
                // Messages wait for the next frame: window reports arrive with every mouse move,
                // and waking for each would cost more than the snow.
                WaitForMultipleObjects(&[wake, timer], false, INFINITE);
            } else {
                MsgWaitForMultipleObjectsEx(Some(&[wake]), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
        }
    }
}

fn start_vehicle(screen: &mut Screen, now: i64, directions: &std::collections::hash_map::RandomState, vehicle: Vehicle, rare: bool) {
    let right = directions.hash_one((now, "direction", &screen.monitor.name)) & 1 == 0;
    let bounds = &screen.monitor.bounds;
    let args = (screen.simulation.width(), bounds.right - bounds.left, bounds.bottom - bounds.top, right);
    match vehicle {
        Vehicle::Plow => screen.plow = Some(Plow::new(args.0, args.1, args.2, args.3)),
        Vehicle::Truck => screen.truck = Some(Truck::new_variant(args.0, args.1, args.2, args.3, rare)),
    }
    screen.wake(now);
}

/// One overlay per display: new displays get snow, removed ones lose theirs, and a display that changed
/// size keeps its snow in a fresh overlay. `rebuild` gives every display a fresh overlay, as when the snow
/// moves behind windows or back.
/// Whether the overlays reach down over the taskbar: behind it, or while it rains, which runs down the taskbar as down a
/// window. Snow and leaves lie on its top edge. What falls is what the screens show, the switch to another kind
/// coming only once the gust has cleared the screen.
fn over_taskbar(settings: &Settings, screens: &[Screen]) -> bool {
    settings.behind_taskbar || screens.first().map_or(settings.modes().contains(&Mode::Rain), |screen| screen.simulation.raining())
}

fn sync_screens(
    gpu: &Gpu,
    settings: &Settings,
    full_screen: &HashSet<String>,
    mut current: Vec<Screen>,
    layer: Option<DesktopLayer>,
    rebuild: bool,
) -> Vec<Screen> {
    let mut result = Vec::new();
    let displays: Vec<_> =
        monitors(over_taskbar(settings, &current), full_screen).into_iter().filter(|monitor| prefs::display_enabled(&monitor.name)).collect();
    for monitor in displays.iter().cloned() {
        let kept = current.iter().position(|screen| screen.monitor.name == monitor.name);
        let simulation = match kept.map(|i| current.swap_remove(i)) {
            Some(screen) if screen.monitor == monitor && !rebuild && screen.behind_windows() == layer.is_some() => {
                result.push(screen);
                continue;
            }
            Some(screen) => {
                let mut simulation = screen.simulation;
                let (width, height) = dip_size(&monitor);
                // Reaching further down, as over a taskbar a full-screen app has put away, the drift falls from
                // where it lay onto the new bottom; stopping short again, what lay along the bottom is under the
                // taskbar. Everything else stays where it is on the screen.
                let drop = monitor.rect.bottom - screen.monitor.rect.bottom;
                let scale = monitor.dpi.max(96) as f32 / 96.0;
                if drop > 0 && monitor.rect.top == screen.monitor.rect.top {
                    simulation.lower_ground(width as f32, height as f32, drop as f32 / scale);
                } else if drop < 0 && monitor.rect.top == screen.monitor.rect.top {
                    simulation.raise_ground(width as f32, height as f32, -drop as f32 / scale);
                } else {
                    simulation.resize(width as f32, height as f32);
                }
                simulation
            }
            None => new_simulation(&monitor, settings),
        };
        log!("display {}: {:?}, {} dpi, {} Hz", monitor.name, monitor.rect, monitor.dpi, monitor.refresh);
        let screen = Screen::new(gpu, monitor.clone(), simulation, settings, layer).or_else(|(error, simulation)| {
            // Snow that can't go behind windows stays on top of them rather than vanishing.
            if layer.is_none() {
                return Err((error, simulation));
            }
            log!("overlay behind windows failed: {error}");
            Screen::new(gpu, monitor, simulation, settings, None)
        });
        match screen {
            Ok(screen) => result.push(screen),
            Err((error, _)) => log!("overlay failed: {error}"),
        }
    }
    for screen in &mut result {
        screen.simulation.walls = walls(&screen.monitor, &displays);
    }
    result
}

fn find_layer() -> Option<DesktopLayer> {
    let layer = desktop::find();
    log!("desktop layer: {layer:?}");
    layer
}

fn new_simulation(monitor: &Monitor, settings: &Settings) -> SnowSimulation {
    let (width, height) = dip_size(monitor);
    let mut simulation = SnowSimulation::new(width as f32, height as f32, settings.intensity as f32 / 100.0);
    simulation.thaws = settings.melt;
    simulation.accumulates = settings.accumulate;
    simulation.falling_opacity = settings.opacity as f32 / 100.0;
    simulation.set_snowfall_held(!settings.welcomed);
    simulation.set_halloween(halloween::shows(settings));
    simulation.set_modes(&settings.modes());
    simulation
}

fn power_budget() -> PowerBudget {
    let mut status = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return PowerBudget::Full;
    }
    if status.SystemStatusFlag == 1 {
        PowerBudget::Saver
    } else if status.ACLineStatus == 0 {
        PowerBudget::Battery
    } else {
        PowerBudget::Full
    }
}

/// What a frame costs on this thread, written to the log twice a minute.
struct Stats {
    since: i64,
    steps: u32,
    draws: u32,
    busy: i64,
    sprites: usize,
}

impl Stats {
    fn new(now: i64) -> Stats {
        Stats { since: now, steps: 0, draws: 0, busy: 0, sprites: 0 }
    }

    fn frame(&mut self, drew: bool, busy: i64, sprites: usize) {
        self.steps += 1;
        self.draws += drew as u32;
        self.busy += busy;
        self.sprites = self.sprites.max(sprites);
    }

    fn report(&mut self, clock: &Clock, now: i64, tracker: &mut Tracker) {
        let elapsed = clock.seconds(now - self.since);
        if elapsed < 30.0 {
            return;
        }
        let busy = clock.seconds(self.busy);
        let listing = clock.seconds(tracker.cost);
        log!(
            "frames: {:.1} steps/s, {:.1} draws/s, {:.0} µs per step, {:.2}% of a core, up to {} sprites; windows: {:.1} reads/s, {:.0} µs per read",
            self.steps as f64 / elapsed,
            self.draws as f64 / elapsed,
            busy * 1e6 / self.steps.max(1) as f64,
            (busy + listing) / elapsed * 100.0,
            self.sprites,
            tracker.listings as f64 / elapsed,
            listing * 1e6 / tracker.listings.max(1) as f64
        );
        *self = Stats::new(now);
        tracker.listings = 0;
        tracker.cost = 0;
    }
}
