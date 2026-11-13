//! A small pixel-art collection window. The simulation remains the owner of every object.
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, Once};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1CreateFactory, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_FAR, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, ScreenToClient,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_TAB,
    VK_U, VK_UP, VK_Z,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Error, HSTRING, PCWSTR, Result, w};
use windows_numerics::Vector2;

use crate::cone::Species;
use crate::gifts::GiftKind;
use crate::snow::{CollectionEntry, CollectionError, CollectionObject};
use snowhush::card;
use snowhush_core::text::tr;

#[path = "inventory_accessibility.rs"]
mod accessibility;

const CLASS: PCWSTR = w!("SnowhushInventory");
const GHOST_CLASS: PCWSTR = w!("SnowhushInventoryDrag");
const WIDTH: f32 = 432.0;
const HEIGHT: f32 = 512.0;
const COLS: usize = 4;
const SLOT: f32 = 88.0;
const LEFT: f32 = 40.0;
const WM_DROP_HOVER: u32 = WM_APP + 51;
const WM_DROP_RESULT: u32 = WM_APP + 52;
static DROP_WINDOW: AtomicIsize = AtomicIsize::new(0);
static DROP_HOVER: AtomicBool = AtomicBool::new(false);
static DROP_RESULTS: Mutex<Vec<(isize, String, std::result::Result<usize, CollectionError>)>> = Mutex::new(Vec::new());

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Trees,
    Toys,
}

impl Category {
    const ALL: [Self; 2] = [Self::Trees, Self::Toys];

    fn index(self) -> usize {
        match self {
            Self::Trees => 0,
            Self::Toys => 1,
        }
    }
    fn top(self) -> f32 {
        match self {
            Self::Trees => 158.0,
            Self::Toys => 307.0,
        }
    }
    fn height(self) -> f32 {
        match self {
            Self::Trees => 112.0,
            Self::Toys => 78.0,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Trees => "collection.trees",
            Self::Toys => "collection.toys",
        }
    }
    fn of(object: CollectionObject) -> Self {
        match object {
            CollectionObject::Tree(..) => Self::Trees,
            CollectionObject::Gift(GiftKind::Lantern | GiftKind::Boombox) => Self::Toys,
        }
    }
}

pub struct Handlers {
    pub snapshot: Box<dyn Fn() -> Vec<CollectionEntry>>,
    pub height_in_feet: bool,
    pub set_height_in_feet: Box<dyn Fn(bool)>,
    pub store: Box<dyn Fn(CollectionEntry) -> std::result::Result<usize, CollectionError>>,
    pub undo: Box<dyn Fn(String) -> std::result::Result<(), CollectionError>>,
    pub place: Box<dyn Fn(CollectionEntry, POINT) -> std::result::Result<(), CollectionError>>,
}

struct Drag {
    index: usize,
    start: POINT,
    moved: bool,
    pixels: Option<Vec<u8>>,
}

struct Inventory {
    window: HWND,
    ghost: HWND,
    grab_cursor: Option<HCURSOR>,
    scale: f32,
    entries: Vec<CollectionEntry>,
    stored: bool,
    offsets: [usize; 2],
    wheel: [i32; 2],
    accessibility: accessibility::Shared,
    accessibility_focus: u32,
    focus_category: Category,
    focus: usize,
    keyboard: bool,
    hover: Option<POINT>,
    height_in_feet: bool,
    drag: Option<Drag>,
    drop_hover: bool,
    message: Option<String>,
    undo_display: Option<String>,
    handlers: Handlers,
}

thread_local! { static INVENTORY: RefCell<Option<Inventory>> = const { RefCell::new(None) }; }

/// The visible part of the collection window, ignoring only our own click-through overlays.
pub fn drop_target_at(point: POINT) -> bool {
    let id = DROP_WINDOW.load(Ordering::Acquire);
    if id == 0 {
        return false;
    }
    let target = HWND(id as _);
    unsafe {
        if !IsWindow(Some(target)).as_bool() || !IsWindowVisible(target).as_bool() || crate::tracker::cloaked(target) {
            return false;
        }
        let mut next = GetTopWindow(None).ok();
        while let Some(window) = next {
            if IsWindowVisible(window).as_bool() && !crate::tracker::cloaked(window) {
                let mut bounds = RECT::default();
                if GetWindowRect(window, &mut bounds).is_ok()
                    && point.x >= bounds.left
                    && point.x < bounds.right
                    && point.y >= bounds.top
                    && point.y < bounds.bottom
                {
                    if window == target {
                        return true;
                    }
                    let mut process = 0;
                    GetWindowThreadProcessId(window, Some(&mut process));
                    let own_overlay = process == std::process::id()
                        && matches!(crate::tracker::class_name(window).as_str(), "SnowhushOverlay" | "SnowhushInventoryDrag");
                    let transparent = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0 != 0;
                    if !own_overlay && !transparent {
                        return false;
                    }
                }
            }
            next = GetWindow(window, GW_HWNDNEXT).ok();
        }
    }
    false
}

pub fn set_drop_hover(over: bool) {
    let id = DROP_WINDOW.load(Ordering::Acquire);
    let over = id != 0 && over;
    if DROP_HOVER.swap(over, Ordering::AcqRel) != over && id != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(id as _)), WM_DROP_HOVER, WPARAM(over as usize), LPARAM(0));
        }
    }
}

pub fn report_drop(display: String, result: std::result::Result<usize, CollectionError>) {
    let id = DROP_WINDOW.load(Ordering::Acquire);
    if id == 0 {
        return;
    }
    if let Ok(mut queue) = DROP_RESULTS.lock() {
        queue.push((id, display, result));
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(id as _)), WM_DROP_RESULT, WPARAM(0), LPARAM(0));
    }
}

#[cfg(test)]
pub(crate) fn test_window() -> HWND {
    HWND(DROP_WINDOW.load(Ordering::Acquire) as _)
}

#[cfg(test)]
pub(crate) fn close_test_window() {
    close_window();
}

#[cfg(test)]
pub(crate) fn test_drop_feedback() -> Option<(bool, Option<String>, Option<String>)> {
    INVENTORY.with_borrow(|state| state.as_ref().map(|state| (state.stored, state.message.clone(), state.undo_display.clone())))
}

impl Inventory {
    fn filtered(&self, category: Category) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, item)| item.stored == self.stored && Category::of(item.object) == category)
            .map(|(i, _)| i)
            .collect()
    }
    fn keyboard_item(&self) -> Option<usize> {
        self.keyboard.then(|| self.filtered(self.focus_category).get(self.focus).copied()).flatten()
    }
    fn size(&self) -> (i32, i32) {
        ((WIDTH * self.scale).ceil() as i32, (HEIGHT * self.scale).ceil() as i32)
    }
    fn hit(&self, point: POINT) -> Option<usize> {
        let x = point.x as f32 / self.scale - LEFT;
        let y = point.y as f32 / self.scale;
        if x < 0.0 {
            return None;
        }
        let col = (x / SLOT) as usize;
        if col >= COLS || x % SLOT >= 78.0 {
            return None;
        }
        let category = Category::ALL.into_iter().find(|category| y >= category.top() && y < category.top() + category.height())?;
        self.filtered(category).get(self.offsets[category.index()] + col).copied()
    }
    fn scroll(&mut self, category: Category, right: bool) {
        let index = category.index();
        let max = self.filtered(category).len().saturating_sub(COLS);
        self.offsets[index] = if right { (self.offsets[index] + 1).min(max) } else { self.offsets[index].saturating_sub(1) };
        self.focus_category = category;
        let last = (self.offsets[index] + COLS - 1).min(self.filtered(category).len().saturating_sub(1));
        self.focus = self.focus.clamp(self.offsets[index], last);
    }
    fn reveal_focus(&mut self) {
        let offset = &mut self.offsets[self.focus_category.index()];
        if self.focus < *offset {
            *offset = self.focus;
        }
        if self.focus >= *offset + COLS {
            *offset = self.focus + 1 - COLS;
        }
    }
    fn refresh(&mut self) {
        self.entries = (self.handlers.snapshot)();
        for category in Category::ALL {
            self.offsets[category.index()] = self.offsets[category.index()].min(self.filtered(category).len().saturating_sub(COLS));
        }
        self.focus = self.focus.min(self.filtered(self.focus_category).len().saturating_sub(1));
    }
    fn move_item(&mut self, index: usize) {
        let item = self.entries[index].clone();
        match (self.handlers.store)(item.clone()) {
            Ok(attached) => {
                if !item.stored {
                    self.message = Some(if attached > 0 {
                        tr("collection.stored_attached").replace("{count}", &attached.to_string())
                    } else {
                        tr("collection.stored_success").to_owned()
                    });
                    self.undo_display = Some(item.monitor);
                } else {
                    self.message = Some(tr("collection.placed_success").to_owned());
                    self.undo_display = None;
                }
            }
            Err(error) => self.message = Some(tr(error_key(error)).to_owned()),
        }
        self.refresh();
    }
    fn undo(&mut self) {
        let Some(display) = self.undo_display.clone() else { return };
        match (self.handlers.undo)(display) {
            Ok(()) => {
                self.message = Some(tr("collection.undo_success").to_owned());
                self.undo_display = None;
            }
            Err(error) => self.message = Some(tr(error_key(error)).to_owned()),
        }
        self.refresh();
    }
}

fn error_key(error: CollectionError) -> &'static str {
    match error {
        CollectionError::WrongDisplay => "collection.error_monitor",
        CollectionError::NoRoom => "collection.error_space",
        CollectionError::Busy => "collection.error_busy",
        CollectionError::Unavailable => "collection.error_unavailable",
        CollectionError::SaveFailed => "collection.error_save",
        CollectionError::Unconfirmed => "collection.error_unconfirmed",
    }
}

pub fn show(handlers: Handlers) -> Result<()> {
    let already = INVENTORY.with_borrow_mut(|state| {
        if let Some(state) = state.as_mut() {
            state.refresh();
            unsafe {
                let _ = ShowWindow(state.window, SW_SHOW);
                let _ = SetForegroundWindow(state.window);
            }
            true
        } else {
            false
        }
    });
    if already {
        repaint();
        return Ok(());
    }
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let _ = card::register(CLASS, Some(procedure));
        let _ = card::register(GHOST_CLASS, Some(ghost_procedure));
    });
    let (scale, origin) = placement();
    let entries = (handlers.snapshot)();
    let height_in_feet = handlers.height_in_feet;
    unsafe {
        let window_style = if cfg!(feature = "inventory-preview") { WS_EX_APPWINDOW } else { WS_EX_TOOLWINDOW };
        let window = CreateWindowExW(
            WS_EX_LAYERED | window_style,
            CLASS,
            w!("VelvetFrost · Collection"),
            WS_POPUP,
            origin.x,
            origin.y,
            (WIDTH * scale) as i32,
            (HEIGHT * scale) as i32,
            None,
            None,
            None,
            None,
        )?;
        let ghost = match CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            GHOST_CLASS,
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        ) {
            Ok(ghost) => ghost,
            Err(error) => {
                let _ = DestroyWindow(window);
                return Err(error);
            }
        };
        INVENTORY.set(Some(Inventory {
            window,
            ghost,
            grab_cursor: grab_cursor(),
            scale,
            entries,
            stored: true,
            offsets: [0; 2],
            wheel: [0; 2],
            accessibility: Default::default(),
            accessibility_focus: 0,
            focus_category: Category::Trees,
            focus: 0,
            keyboard: false,
            hover: None,
            height_in_feet,
            drag: None,
            drop_hover: false,
            message: None,
            undo_display: None,
            handlers,
        }));
        DROP_HOVER.store(false, Ordering::Release);
        DROP_WINDOW.store(window.0 as isize, Ordering::Release);
        repaint();
        let _ = ShowWindow(window, SW_SHOW);
        // Preview processes may inherit a hidden startup show state from the test runner.
        #[cfg(feature = "inventory-preview")]
        let _ = ShowWindow(window, SW_SHOW);
        SetTimer(Some(window), 1, 2000, None);
        let _ = SetForegroundWindow(window);
    }
    Ok(())
}

fn fitted_scale(scale: f32, work: RECT) -> f32 {
    let width = (work.right - work.left - 16).max(1) as f32;
    let height = (work.bottom - work.top - 16).max(1) as f32;
    scale.min(width / WIDTH).min(height / HEIGHT)
}

fn placement() -> (f32, POINT) {
    let fallback = card::placement(WIDTH, HEIGHT);
    unsafe {
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_err() {
            return fallback;
        }
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return fallback;
        }
        let work = info.rcWork;
        let scale = fitted_scale(fallback.0, work);
        let width = (WIDTH * scale).ceil() as i32;
        let height = (HEIGHT * scale).ceil() as i32;
        (scale, POINT { x: work.left + (work.right - work.left - width) / 2, y: work.top + (work.bottom - work.top - height) / 2 })
    }
}

unsafe extern "system" fn ghost_procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_DROP_HOVER => {
            INVENTORY.with_borrow_mut(|state| {
                if let Some(state) = state {
                    state.drop_hover = wparam.0 != 0;
                }
            });
            repaint();
            LRESULT(0)
        }
        WM_DROP_RESULT => {
            let results = DROP_RESULTS.lock().map(|mut queue| std::mem::take(&mut *queue)).unwrap_or_default();
            INVENTORY.with_borrow_mut(|state| {
                let Some(state) = state else { return };
                for (id, display, result) in results {
                    if id != window.0 as isize {
                        continue;
                    }
                    state.drop_hover = false;
                    match result {
                        Ok(attached) => {
                            state.stored = true;
                            state.offsets = [0; 2];
                            state.message = Some(if attached > 0 {
                                tr("collection.stored_attached").replace("{count}", &attached.to_string())
                            } else {
                                tr("collection.stored_success").to_owned()
                            });
                            state.undo_display = Some(display);
                        }
                        Err(error) => state.message = Some(tr(error_key(error)).to_owned()),
                    }
                    state.refresh();
                }
            });
            repaint();
            LRESULT(0)
        }
        WM_GETOBJECT if lparam.0 as i32 == windows::Win32::UI::Accessibility::UiaRootObjectId => {
            let shared = INVENTORY.with(|cell| cell.try_borrow().ok().and_then(|state| state.as_ref().map(|state| state.accessibility.clone())));
            if let Some(shared) = shared {
                let provider = accessibility::root(window, &shared);
                return unsafe { windows::Win32::UI::Accessibility::UiaReturnRawElementProvider(window, wparam, lparam, &provider) };
            }
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        accessibility::INVOKE | accessibility::FOCUS => {
            let id = wparam.0 as u32;
            let generation = (wparam.0 >> 32) as u32;
            let mut close = false;
            INVENTORY.with_borrow_mut(|state| {
                let Some(state) = state.as_mut() else { return };
                let valid = state
                    .accessibility
                    .lock()
                    .ok()
                    .is_some_and(|snapshot| snapshot.generation == generation && snapshot.nodes.iter().any(|node| node.id == id && node.enabled));
                if !valid {
                    return;
                }
                state.accessibility_focus = id;
                if id >= 100 {
                    let category = if id < 104 { Category::Trees } else { Category::Toys };
                    let slot = (id - if category == Category::Trees { 100 } else { 104 }) as usize;
                    let position = state.offsets[category.index()] + slot;
                    state.focus_category = category;
                    state.focus = position;
                    state.keyboard = true;
                    if message == accessibility::INVOKE {
                        if let Some(index) = state.filtered(category).get(position).copied() {
                            state.move_item(index);
                        }
                    }
                } else if message == accessibility::INVOKE {
                    match id {
                        1 | 2 => {
                            state.stored = id == 1;
                            state.offsets = [0; 2];
                            state.focus = 0;
                            state.focus_category = Category::Trees;
                        }
                        3 | 4 => {
                            state.height_in_feet = id == 4;
                            (state.handlers.set_height_in_feet)(state.height_in_feet);
                        }
                        5 | 6 => state.scroll(Category::Trees, id == 6),
                        7 | 8 => state.scroll(Category::Toys, id == 8),
                        9 => state.undo(),
                        10 => close = true,
                        _ => {}
                    }
                }
            });
            if close {
                close_window();
            } else {
                repaint();
                if message == accessibility::FOCUS {
                    unsafe {
                        let _ = SetFocus(Some(window));
                    }
                    announce_accessibility_focus();
                }
            }
            LRESULT(0)
        }
        WM_MOVE => {
            INVENTORY.with(|cell| {
                if let Ok(state) = cell.try_borrow() {
                    if let Some(state) = state.as_ref() {
                        update_accessibility(state);
                    }
                }
            });
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_NCHITTEST => {
            let (x, y) = INVENTORY.with_borrow(|state| {
                state
                    .as_ref()
                    .map(|state| {
                        let mut rect = windows::Win32::Foundation::RECT::default();
                        unsafe {
                            let _ = GetWindowRect(window, &mut rect);
                        }
                        let point = card::point(lparam);
                        ((point.x - rect.left) as f32 / state.scale, (point.y - rect.top) as f32 / state.scale)
                    })
                    .unwrap_or((100.0, 100.0))
            });
            LRESULT(if y < 49.0 && x < WIDTH - 50.0 { HTCAPTION as isize } else { HTCLIENT as isize })
        }
        WM_LBUTTONDOWN => {
            let point = card::point(lparam);
            let mut capture = false;
            let mut close = false;
            INVENTORY.with_borrow_mut(|state| {
                let Some(state) = state.as_mut() else { return };
                let x = point.x as f32 / state.scale;
                let y = point.y as f32 / state.scale;
                state.keyboard = false;
                if x > WIDTH - 45.0 && y < 75.0 {
                    close = true;
                    return;
                }
                if (445.0..470.0).contains(&y) && (320.0..408.0).contains(&x) && state.undo_display.is_some() {
                    state.undo();
                } else if (92.0..128.0).contains(&y) {
                    if (40.0..210.0).contains(&x) {
                        state.stored = true;
                        state.offsets = [0; 2];
                        state.focus_category = Category::Trees;
                        state.focus = 0;
                    }
                    if (218.0..392.0).contains(&x) {
                        state.stored = false;
                        state.offsets = [0; 2];
                        state.focus_category = Category::Trees;
                        state.focus = 0;
                    }
                } else if (135.0..156.0).contains(&y) && (312.0..392.0).contains(&x) {
                    state.height_in_feet = x >= 352.0;
                    (state.handlers.set_height_in_feet)(state.height_in_feet);
                } else if let Some(category) = Category::ALL
                    .into_iter()
                    .find(|category| y >= category.top() && y < category.top() + category.height() && (x < LEFT || x >= WIDTH - LEFT))
                {
                    state.scroll(category, x >= WIDTH - LEFT);
                } else if let Some(index) = state.hit(point) {
                    if state.stored {
                        let mut cursor = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut cursor);
                        }
                        state.drag = Some(Drag { index, start: cursor, moved: false, pixels: None });
                        capture = true;
                    } else {
                        state.move_item(index);
                    }
                }
            });
            if close {
                close_window();
                return LRESULT(0);
            }
            if capture {
                unsafe {
                    SetCapture(window);
                }
            }
            repaint();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut tracking = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: window, dwHoverTime: 0 };
            let mut cursor = POINT::default();
            unsafe {
                let _ = TrackMouseEvent(&mut tracking);
                let _ = GetCursorPos(&mut cursor);
            }
            let (ghost, hover_changed) = INVENTORY.with_borrow_mut(|state| {
                let Some(state) = state.as_mut() else { return (None, false) };
                let previous = state.hover.and_then(|point| state.hit(point));
                let ghost = if let Some(drag) = state.drag.as_mut() {
                    if !drag.moved && (cursor.x - drag.start.x).abs() + (cursor.y - drag.start.y).abs() >= 6 {
                        drag.moved = true;
                    }
                    if drag.moved && drag.pixels.is_none() {
                        drag.pixels = ghost_pixels(state.scale, state.entries[drag.index].object).ok();
                    }
                    if drag.moved {
                        unsafe {
                            SetCursor(state.grab_cursor.or_else(|| LoadCursorW(None, IDC_HAND).ok()));
                        }
                    }
                    drag.pixels.as_ref().map(|pixels| (state.ghost, state.scale, pixels.clone()))
                } else {
                    None
                };
                let point = card::point(lparam);
                state.hover = if state.drag.as_ref().is_some_and(|drag| drag.moved) || state.hit(point).is_none() { None } else { Some(point) };
                (ghost, previous != state.hover.and_then(|point| state.hit(point)))
            });
            if let Some((window, scale, pixels)) = ghost {
                show_ghost(window, scale, &pixels, cursor);
            }
            if hover_changed {
                repaint();
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let changed = INVENTORY.with_borrow_mut(|state| state.as_mut().is_some_and(|state| state.hover.take().is_some()));
            if changed {
                repaint();
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let mut point = card::point(lparam);
            unsafe {
                let _ = ScreenToClient(window, &mut point);
            }
            let delta = ((wparam.0 >> 16) as u16 as i16) as i32 * if message == WM_MOUSEWHEEL { -1 } else { 1 };
            let changed = INVENTORY.with_borrow_mut(|state| {
                let Some(state) = state.as_mut().filter(|state| state.drag.is_none()) else { return false };
                let y = point.y as f32 / state.scale;
                let Some(category) = Category::ALL.into_iter().find(|category| y >= category.top() && y < category.top() + category.height()) else {
                    return false;
                };
                let i = category.index();
                state.wheel[i] += delta;
                let steps = state.wheel[i] / 120;
                state.wheel[i] %= 120;
                let before = state.offsets[i];
                for _ in 0..steps.unsigned_abs().min(20) {
                    state.scroll(category, steps > 0);
                }
                state.offsets[i] != before
            });
            if changed {
                repaint();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut cursor = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cursor);
            }
            let drag = INVENTORY.with_borrow_mut(|state| {
                let state = state.as_mut()?;
                let drag = state.drag.take()?;
                unsafe {
                    let _ = ShowWindow(state.ghost, SW_HIDE);
                }
                let mut rect = windows::Win32::Foundation::RECT::default();
                unsafe {
                    let _ = GetWindowRect(state.window, &mut rect);
                }
                if dropped_outside(drag.moved, cursor, rect) {
                    state.message = Some(match (state.handlers.place)(state.entries[drag.index].clone(), cursor) {
                        Ok(()) => {
                            state.undo_display = None;
                            tr("collection.placed_success").to_owned()
                        }
                        Err(error) => tr(error_key(error)).to_owned(),
                    });
                    state.refresh();
                }
                let point = card::point(lparam);
                state.hover = state.hit(point).map(|_| point);
                Some(())
            });
            if drag.is_some() {
                unsafe {
                    SetCursor(LoadCursorW(None, IDC_ARROW).ok());
                    let _ = ReleaseCapture();
                }
                repaint();
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            let dragging = INVENTORY.with_borrow_mut(|state| {
                if let Some(state) = state.as_mut() {
                    let dragging = state.drag.take().is_some();
                    unsafe {
                        let _ = ShowWindow(state.ghost, SW_HIDE);
                    }
                    dragging
                } else {
                    false
                }
            });
            if dragging {
                unsafe {
                    SetCursor(LoadCursorW(None, IDC_ARROW).ok());
                }
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let cursor = INVENTORY.with_borrow(|state| {
                state.as_ref().filter(|state| state.drag.as_ref().is_some_and(|drag| drag.moved)).map(|state| state.grab_cursor)
            });
            if let Some(cursor) = cursor {
                unsafe {
                    SetCursor(cursor.or_else(|| LoadCursorW(None, IDC_HAND).ok()));
                }
                LRESULT(1)
            } else {
                unsafe { DefWindowProcW(window, message, wparam, lparam) }
            }
        }
        WM_TIMER if wparam.0 == 1 => {
            let changed = INVENTORY.with_borrow_mut(|state| {
                let Some(state) = state.as_mut() else { return false };
                if state.drag.is_some() {
                    return false;
                }
                let previous = state.entries.clone();
                state.refresh();
                previous != state.entries
            });
            if changed {
                repaint();
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            let dragging = INVENTORY.with_borrow_mut(|state| {
                let Some(state) = state.as_mut() else { return false };
                let dragging = state.drag.take().is_some();
                if dragging {
                    unsafe {
                        let _ = ShowWindow(state.ghost, SW_HIDE);
                    }
                }
                dragging
            });
            if dragging {
                unsafe {
                    SetCursor(LoadCursorW(None, IDC_ARROW).ok());
                    let _ = ReleaseCapture();
                }
            } else {
                close_window();
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_U.0 as usize => {
            INVENTORY.with_borrow_mut(|state| {
                if let Some(state) = state.as_mut().filter(|state| state.drag.is_none()) {
                    state.height_in_feet = !state.height_in_feet;
                    (state.handlers.set_height_in_feet)(state.height_in_feet);
                }
            });
            repaint();
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_Z.0 as usize => {
            INVENTORY.with_borrow_mut(|state| {
                if let Some(state) = state.as_mut().filter(|state| state.drag.is_none()) {
                    state.undo();
                }
            });
            repaint();
            LRESULT(0)
        }
        WM_KEYDOWN if [VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0, VK_RETURN.0, VK_TAB.0].contains(&(wparam.0 as u16)) => {
            INVENTORY.with_borrow_mut(|state| {
                let Some(state) = state.as_mut() else { return };
                if state.drag.is_some() {
                    return;
                }
                let key = wparam.0 as u16;
                if key == VK_TAB.0 {
                    state.keyboard = false;
                    state.stored = !state.stored;
                    state.offsets = [0; 2];
                    state.focus_category = Category::Trees;
                    state.focus = 0;
                    state.accessibility_focus = if state.stored { 1 } else { 2 };
                } else if key == VK_RETURN.0 {
                    if let Some(index) = state.keyboard_item() {
                        state.move_item(index);
                    }
                } else {
                    let was_keyboard = state.keyboard;
                    state.keyboard = true;
                    if key == VK_UP.0 {
                        state.focus_category = Category::Trees;
                    }
                    if key == VK_DOWN.0 {
                        state.focus_category = Category::Toys;
                    }
                    let len = state.filtered(state.focus_category).len();
                    if len > 0 {
                        state.focus = if !was_keyboard {
                            state.offsets[state.focus_category.index()].min(len - 1)
                        } else if key == VK_LEFT.0 {
                            state.focus.saturating_sub(1)
                        } else if key == VK_RIGHT.0 {
                            (state.focus + 1).min(len - 1)
                        } else {
                            state.focus.min(len - 1)
                        };
                        state.reveal_focus();
                        state.accessibility_focus = (if state.focus_category == Category::Trees { 100 } else { 104 })
                            + (state.focus - state.offsets[state.focus_category.index()]) as u32;
                    } else {
                        state.focus = 0;
                        state.accessibility_focus = 0;
                    }
                }
            });
            repaint();
            announce_accessibility_focus();
            LRESULT(0)
        }
        WM_CLOSE => {
            close_window();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn dropped_outside(moved: bool, point: POINT, window: RECT) -> bool {
    moved && (point.x < window.left || point.x >= window.right || point.y < window.top || point.y >= window.bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn collection_is_a_normal_window_and_only_visible_area_accepts_a_drop() {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        show(Handlers {
            snapshot: Box::new(Vec::new),
            height_in_feet: false,
            set_height_in_feet: Box::new(|_| {}),
            store: Box::new(|_| Ok(0)),
            undo: Box::new(|_| Ok(())),
            place: Box::new(|_, _| Ok(())),
        })
        .expect("show collection");
        let window = HWND(DROP_WINDOW.load(Ordering::Acquire) as _);
        let mut bounds = RECT::default();
        unsafe {
            assert_eq!(GetWindowLongPtrW(window, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0, 0);
            GetWindowRect(window, &mut bounds).expect("collection bounds");
            SetWindowPos(window, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE).expect("raise collection");
        }
        let point = POINT { x: bounds.left + 30, y: bounds.top + 30 };
        assert!(drop_target_at(point));
        let blocker = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("test blocker"),
                WS_POPUP | WS_VISIBLE,
                point.x - 10,
                point.y - 10,
                40,
                40,
                None,
                None,
                None,
                None,
            )
            .expect("create covering window")
        };
        unsafe {
            SetWindowPos(blocker, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE).expect("raise covering window");
        }
        assert!(!drop_target_at(point), "a window opened later must cover the collection");
        unsafe {
            DestroyWindow(blocker).expect("destroy blocker");
        }
        assert!(drop_target_at(point));
        close_window();
        assert!(!drop_target_at(point));
    }

    #[test]
    fn translated_collection_names_fit_in_two_lines() {
        let tables = [
            include_str!("../../locales/en.strings"),
            include_str!("../../locales/de.strings"),
            include_str!("../../locales/es.strings"),
            include_str!("../../locales/fr.strings"),
            include_str!("../../locales/ja.strings"),
            include_str!("../../locales/pt.strings"),
            include_str!("../../locales/ru.strings"),
            include_str!("../../locales/uk.strings"),
            include_str!("../../locales/zh-Hans.strings"),
        ];
        let keys = ["spruce", "pine", "fir", "celestial", "ruby", "rose", "lantern", "boombox"];
        let write: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.expect("create DirectWrite factory");
        for table in tables {
            for key in keys {
                let prefix = format!("\"collection.{key}\"");
                let line = table.lines().find(|line| line.trim_start().starts_with(&prefix)).expect("translated name");
                let value = line.split('"').nth(3).expect("quoted name");
                let layout = label_layout(&write, value, 70.0, 27.0).expect("layout name");
                let mut metrics = DWRITE_TEXT_METRICS::default();
                unsafe { layout.GetMetrics(&mut metrics).expect("measure name") };
                assert!(metrics.lineCount <= 2 && metrics.height <= 27.0 && metrics.width <= 70.0, "name does not fit: {value}");
            }
        }
    }

    #[test]
    #[ignore = "writes an isolated preview image to the temporary directory"]
    fn collection_label_preview() {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        snowhush_core::text::choose(&["ru"]);
        let mut entries: Vec<_> = [crate::sculpt::CELESTIAL_VARIANT, 0, crate::sculpt::ROSE_VARIANT, crate::sculpt::RUBY_VARIANT]
            .into_iter()
            .enumerate()
            .map(|(id, variant)| CollectionEntry {
                monitor: "DISPLAY1".into(),
                object: CollectionObject::Tree(id as u64, Species::Spruce, variant),
                stored: true,
                tree_height_mm: Some(2090),
            })
            .collect();
        for kind in [GiftKind::Boombox, GiftKind::Lantern] {
            entries.push(CollectionEntry { monitor: "DISPLAY1".into(), object: CollectionObject::Gift(kind), stored: true, tree_height_mm: None });
        }
        let state = inventory(entries);
        let pixels = paint(&state).expect("render preview");
        let (width, height) = state.size();
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(54u32 + pixels.len() as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&width.to_le_bytes());
        bmp.extend_from_slice(&(-height).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 24]);
        bmp.extend_from_slice(&pixels);
        let path = std::env::temp_dir().join(format!("velvetfrost-collection-preview-{}.bmp", std::process::id()));
        std::fs::write(&path, bmp).expect("write preview");
        println!("{}", path.display());
    }

    fn inventory(entries: Vec<CollectionEntry>) -> Inventory {
        Inventory {
            window: HWND::default(),
            ghost: HWND::default(),
            grab_cursor: None,
            scale: 1.0,
            entries,
            stored: true,
            offsets: [0; 2],
            wheel: [0; 2],
            accessibility: Default::default(),
            accessibility_focus: 0,
            focus_category: Category::Trees,
            focus: 0,
            keyboard: false,
            hover: None,
            drag: None,
            drop_hover: false,
            message: None,
            height_in_feet: false,
            undo_display: None,
            handlers: Handlers {
                snapshot: Box::new(Vec::new),
                height_in_feet: false,
                set_height_in_feet: Box::new(|_| {}),
                store: Box::new(|_| Ok(0)),
                undo: Box::new(|_| Ok(())),
                place: Box::new(|_, _| Err(CollectionError::Unavailable)),
            },
        }
    }

    #[test]
    fn category_ribbons_scroll_independently_and_hit_their_own_cells() {
        let mut entries = Vec::new();
        for id in 0..6 {
            entries.push(CollectionEntry {
                monitor: "DISPLAY1".into(),
                object: CollectionObject::Tree(id, Species::Spruce, 0),
                stored: true,
                tree_height_mm: Some(1500),
            });
            entries.push(CollectionEntry {
                monitor: "DISPLAY1".into(),
                object: CollectionObject::Gift(GiftKind::Lantern),
                stored: true,
                tree_height_mm: None,
            });
        }
        entries.push(CollectionEntry {
            monitor: "DISPLAY1".into(),
            object: CollectionObject::Tree(6, Species::Pine, 0),
            stored: false,
            tree_height_mm: Some(1900),
        });
        let mut state = inventory(entries);
        assert_eq!(state.hit(POINT { x: 41, y: 268 }), Some(0));
        assert_eq!(state.hit(POINT { x: 41, y: 308 }), Some(1));
        assert_eq!(state.hit(POINT { x: 41, y: 290 }), None);
        state.scroll(Category::Trees, true);
        assert_eq!(state.hit(POINT { x: 41, y: 160 }), Some(2));
        assert_eq!(state.hit(POINT { x: 41, y: 308 }), Some(1));
        for _ in 0..10 {
            state.scroll(Category::Trees, true);
        }
        assert_eq!(state.offsets, [2, 0]);
        state.scroll(Category::Toys, true);
        assert_eq!(state.offsets, [2, 1]);
        state.stored = false;
        state.offsets = [0; 2];
        assert_eq!(state.hit(POINT { x: 41, y: 160 }), Some(12));
        assert_eq!(state.hit(POINT { x: 41, y: 308 }), None);
    }

    #[test]
    fn releases_inside_cancel_and_only_a_drag_outside_places() {
        let window = RECT { left: -100, top: 40, right: 332, bottom: 470 };
        assert!(!dropped_outside(false, POINT { x: 500, y: 200 }, window));
        assert!(!dropped_outside(true, POINT { x: 0, y: 200 }, window));
        assert!(dropped_outside(true, POINT { x: 332, y: 200 }, window));
        assert!(dropped_outside(true, POINT { x: -150, y: 200 }, window));
    }

    #[test]
    fn height_units_convert_the_same_tree_without_changing_its_size() {
        assert_eq!(height_value(1524, false), 1.524);
        assert!((height_value(1524, true) - 5.0).abs() < 1e-10);
        assert_ne!(height_label(1350, false), height_label(1940, false));
    }

    #[test]
    fn move_and_undo_only_report_success_when_handlers_confirm_it() {
        use std::rc::Rc;
        let entry = CollectionEntry {
            monitor: "DISPLAY1".into(),
            object: CollectionObject::Tree(1, Species::Spruce, 0),
            stored: false,
            tree_height_mm: Some(1500),
        };
        let entries = Rc::new(RefCell::new(vec![entry.clone()]));
        let snapshot = entries.clone();
        let store = entries.clone();
        let mut state = inventory(vec![entry]);
        state.stored = false;
        state.handlers.snapshot = Box::new(move || snapshot.borrow().clone());
        state.handlers.store = Box::new(move |_| {
            store.borrow_mut()[0].stored = true;
            Ok(1)
        });
        state.move_item(0);
        assert!(state.entries[0].stored);
        assert_eq!(state.undo_display.as_deref(), Some("DISPLAY1"));
        assert!(state.message.as_ref().unwrap().contains('1'));
        state.handlers.undo = Box::new(|_| Err(CollectionError::NoRoom));
        state.undo();
        assert!(state.entries[0].stored);
        assert_eq!(state.undo_display.as_deref(), Some("DISPLAY1"));
        assert_eq!(state.message.as_deref(), Some(tr("collection.error_space")));
        let undo = entries.clone();
        state.handlers.undo = Box::new(move |_| {
            undo.borrow_mut()[0].stored = false;
            Ok(())
        });
        state.undo();
        assert!(!state.entries[0].stored);
        assert!(state.undo_display.is_none());
    }

    #[test]
    fn keyboard_move_requires_a_focused_item() {
        let entry = CollectionEntry {
            monitor: "DISPLAY1".into(),
            object: CollectionObject::Tree(1, Species::Spruce, 0),
            stored: true,
            tree_height_mm: Some(1500),
        };
        let mut state = inventory(vec![entry]);
        assert!(state.keyboard_item().is_none());
        state.keyboard = true;
        assert_eq!(state.keyboard_item(), Some(0));
        state.focus_category = Category::Toys;
        assert!(state.keyboard_item().is_none());
    }

    #[test]
    fn inventory_fits_a_small_work_area_without_changing_normal_scale() {
        let large = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        assert_eq!(fitted_scale(1.5, large), 1.5);
        let small = RECT { left: 0, top: 0, right: 1366, bottom: 728 };
        let scale = fitted_scale(1.5, small);
        assert!(scale < 1.5);
        assert!((HEIGHT * scale).ceil() <= 712.0);
        assert!((WIDTH * scale).ceil() <= 1350.0);
    }
}

fn close_window() {
    let state = INVENTORY.take();
    if let Some(state) = state {
        let _ = DROP_WINDOW.compare_exchange(state.window.0 as isize, 0, Ordering::AcqRel, Ordering::Acquire);
        DROP_HOVER.store(false, Ordering::Release);
        if let Ok(mut queue) = DROP_RESULTS.lock() {
            queue.retain(|(id, _, _)| *id != state.window.0 as isize);
        }
        if let Ok(mut snapshot) = state.accessibility.lock() {
            snapshot.nodes.clear();
            snapshot.generation = snapshot.generation.wrapping_add(1);
        }
        unsafe {
            if state.drag.is_some() {
                SetCursor(LoadCursorW(None, IDC_ARROW).ok());
            }
            let _ = ReleaseCapture();
            let _ = DestroyWindow(state.ghost);
            let _ = DestroyWindow(state.window);
            if let Some(cursor) = state.grab_cursor {
                let _ = DestroyCursor(cursor);
            }
        }
    }
}

fn announce_accessibility_focus() {
    let current = INVENTORY.with_borrow(|state| state.as_ref().map(|state| (state.window, state.accessibility.clone(), state.accessibility_focus)));
    if let Some((window, shared, id)) = current {
        accessibility::announce_focus(window, &shared, id);
    }
}

fn repaint() {
    let painted = INVENTORY.with_borrow(|state| {
        let state = state.as_ref()?;
        update_accessibility(state);
        match paint(state) {
            Ok(pixels) => Some((state.window, state.size(), pixels)),
            Err(error) => {
                crate::log!("collection paint failed: {error}");
                None
            }
        }
    });
    if let Some((window, size, pixels)) = painted {
        card::present(window, size, &pixels, None);
    }
}

fn update_accessibility(state: &Inventory) {
    use accessibility::Node;
    let mut window = RECT::default();
    if unsafe { GetWindowRect(state.window, &mut window) }.is_err() {
        return;
    }
    let bounds = |x: f32, y: f32, width: f32, height: f32| {
        [
            window.left as f64 + (x * state.scale) as f64,
            window.top as f64 + (y * state.scale) as f64,
            (width * state.scale) as f64,
            (height * state.scale) as f64,
        ]
    };
    let status = |selected: bool| if selected { tr("collection.selected").to_owned() } else { String::new() };
    let selected_name = |name: &str, selected: bool| if selected { format!("{} · {}", name, tr("collection.selected")) } else { name.to_owned() };
    let mut nodes = vec![
        Node {
            id: 1,
            name: selected_name(tr("collection.tab_stored"), state.stored),
            status: status(state.stored),
            enabled: true,
            bounds: bounds(40.0, 93.0, 174.0, 34.0),
        },
        Node {
            id: 2,
            name: selected_name(tr("collection.tab_placed"), !state.stored),
            status: status(!state.stored),
            enabled: true,
            bounds: bounds(218.0, 93.0, 174.0, 34.0),
        },
        Node {
            id: 3,
            name: selected_name(&format!("{}: {}", tr("collection.units"), tr("collection.unit_m")), !state.height_in_feet),
            status: status(!state.height_in_feet),
            enabled: true,
            bounds: bounds(312.0, 135.0, 38.0, 20.0),
        },
        Node {
            id: 4,
            name: selected_name(&format!("{}: {}", tr("collection.units"), tr("collection.unit_ft")), state.height_in_feet),
            status: status(state.height_in_feet),
            enabled: true,
            bounds: bounds(352.0, 135.0, 38.0, 20.0),
        },
    ];
    for category in Category::ALL {
        let offset = state.offsets[category.index()];
        let items = state.filtered(category);
        let arrow_y = category.top() + (category.height() - 32.0) / 2.0;
        let base_id = if category == Category::Trees { 5 } else { 7 };
        nodes.push(Node {
            id: base_id,
            name: format!("{} · {}", tr(category.label()), tr("collection.previous")),
            status: String::new(),
            enabled: offset > 0,
            bounds: bounds(12.0, arrow_y, 26.0, 32.0),
        });
        for slot in 0..COLS {
            let Some(&index) = items.get(offset + slot) else { continue };
            let item = &state.entries[index];
            let height = item.tree_height_mm.map(|mm| format!(" · {}", height_label(mm, state.height_in_feet))).unwrap_or_default();
            let label = format!(
                "{}{} · {} · {}",
                name(item.object),
                height,
                monitor_label(&item.monitor),
                tr(if state.stored { "collection.drag_hint" } else { "collection.store_hint" })
            );
            nodes.push(Node {
                id: (if category == Category::Trees { 100 } else { 104 }) + slot as u32,
                name: label,
                status: String::new(),
                enabled: true,
                bounds: bounds(LEFT + slot as f32 * SLOT, category.top(), 78.0, category.height()),
            });
        }
        nodes.push(Node {
            id: base_id + 1,
            name: format!("{} · {}", tr(category.label()), tr("collection.next")),
            status: String::new(),
            enabled: offset + COLS < items.len(),
            bounds: bounds(394.0, arrow_y, 26.0, 32.0),
        });
    }
    if state.undo_display.is_some() {
        nodes.push(Node {
            id: 9,
            name: tr("collection.undo").into(),
            status: String::new(),
            enabled: true,
            bounds: bounds(320.0, 445.0, 88.0, 25.0),
        });
    }
    nodes.push(Node { id: 10, name: tr("collection.close").into(), status: String::new(), enabled: true, bounds: bounds(378.0, 24.0, 33.0, 39.0) });
    if let Ok(mut snapshot) = state.accessibility.lock() {
        if snapshot.nodes != nodes {
            snapshot.generation = snapshot.generation.wrapping_add(1);
            snapshot.nodes = nodes;
        }
        snapshot.focus = if snapshot.nodes.iter().any(|node| node.id == state.accessibility_focus) { state.accessibility_focus } else { 0 };
        snapshot.name = tr("collection").to_owned();
        snapshot.window_bounds = [window.left as f64, window.top as f64, (window.right - window.left) as f64, (window.bottom - window.top) as f64];
    }
}

fn ghost_pixels(scale: f32, object: CollectionObject) -> Result<Vec<u8>> {
    let side = (88.0 * scale).ceil() as i32;
    render((side, side), scale, |target, imaging, _| unsafe {
        target.Clear(Some(&D2D1_COLOR_F::default()));
        image(target, imaging, object, rect(4.0, 4.0, 84.0, 84.0))
    })
}

fn show_ghost(window: HWND, scale: f32, pixels: &[u8], cursor: POINT) {
    let side = (88.0 * scale).ceil() as i32;
    card::present(window, (side, side), pixels, Some(POINT { x: cursor.x - side / 2, y: cursor.y - side / 2 }));
    unsafe {
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(window, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// A pixel-art closed hand, with its grip centred on the drop point.
fn grab_cursor() -> Option<HCURSOR> {
    const HAND: [&[u8; 16]; 16] = [
        b"                ",
        b"                ",
        b"    XXX XXX     ",
        b"   X...X...XXX  ",
        b"   X...X...X..X ",
        b"   X...X...X..X ",
        b"  XX...X...X..X ",
        b" X..XXXXXXXX..X ",
        b" X............X ",
        b" X..XXX.......X ",
        b"  X...X.......X ",
        b"   X..........X ",
        b"    X........X  ",
        b"     X......X   ",
        b"     XXXXXXXX   ",
        b"                ",
    ];
    let mut and = [0xffu8; 128];
    let mut xor = [0u8; 128];
    for y in 0..32 {
        for x in 0..32 {
            let pixel = HAND[y / 2][x / 2];
            let offset = y * 4 + x / 8;
            let bit = 0x80 >> (x % 8);
            if pixel != b' ' {
                and[offset] &= !bit;
                if pixel == b'.' {
                    xor[offset] |= bit;
                }
            }
        }
    }
    unsafe { CreateCursor(None, 16, 16, 32, 32, and.as_ptr().cast(), xor.as_ptr().cast()).ok() }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}
fn color(rgb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((rgb >> 16) & 255) as f32 / 255.0, g: ((rgb >> 8) & 255) as f32 / 255.0, b: (rgb & 255) as f32 / 255.0, a: 1.0 }
}

fn render(
    size: (i32, i32),
    scale: f32,
    draw: impl FnOnce(&ID2D1RenderTarget, &IWICImagingFactory, &IDWriteFactory) -> Result<()>,
) -> Result<Vec<u8>> {
    unsafe {
        let imaging: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let d2d = D2D1CreateFactory::<windows::Win32::Graphics::Direct2D::ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let bitmap = imaging.CreateBitmap(size.0 as u32, size.1 as u32, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0 * scale,
            dpiY: 96.0 * scale,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let target = d2d.CreateWicBitmapRenderTarget(&bitmap, &properties)?;
        target.BeginDraw();
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        draw(&target, &imaging, &write)?;
        target.EndDraw(None, None)?;
        drop(target);
        let mut pixels = vec![0; (size.0 * size.1 * 4) as usize];
        bitmap.CopyPixels(std::ptr::null(), (size.0 * 4) as u32, &mut pixels)?;
        Ok(pixels)
    }
}

fn fill(target: &ID2D1RenderTarget, area: D2D_RECT_F, rgb: u32) -> Result<()> {
    unsafe {
        let brush = target.CreateSolidColorBrush(&color(rgb), None)?;
        target.FillRectangle(&area, &brush);
    }
    Ok(())
}

fn text(
    target: &ID2D1RenderTarget,
    write: &IDWriteFactory,
    value: &str,
    area: D2D_RECT_F,
    size: f32,
    rgb: u32,
    centered: bool,
    bold: bool,
) -> Result<()> {
    unsafe {
        let format = write.CreateTextFormat(
            w!("Segoe UI"),
            None,
            if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL },
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            &HSTRING::from(snowhush_core::text::code()),
        )?;
        if centered {
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        }
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let units: Vec<_> = value.encode_utf16().collect();
        let layout = write.CreateTextLayout(&units, &format, area.right - area.left, area.bottom - area.top)?;
        let brush = target.CreateSolidColorBrush(&color(rgb), None)?;
        target.DrawTextLayout(Vector2 { X: area.left, Y: area.top }, &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_CLIP);
    }
    Ok(())
}

/// Keep the whole label within two lines, including in languages with longer names.
fn label_layout(write: &IDWriteFactory, value: &str, width: f32, height: f32) -> Result<IDWriteTextLayout> {
    unsafe {
        let units: Vec<_> = value.encode_utf16().collect();
        for step in 0..=8 {
            let size = 10.0 - step as f32 * 0.5;
            let format = write.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from(snowhush_core::text::code()),
            )?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_FAR)?;
            let layout = write.CreateTextLayout(&units, &format, width, height)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics)?;
            if (metrics.lineCount <= 2 && metrics.height <= height && metrics.width <= width) || step == 8 {
                return Ok(layout);
            }
        }
        unreachable!()
    }
}

fn outlined_label(target: &ID2D1RenderTarget, write: &IDWriteFactory, value: &str, area: D2D_RECT_F) -> Result<()> {
    unsafe {
        let layout = label_layout(write, value, area.right - area.left, area.bottom - area.top)?;
        let outline = target.CreateSolidColorBrush(&color(0x181514), None)?;
        let foreground = target.CreateSolidColorBrush(&color(0xf8eddf), None)?;
        for (dx, dy) in [(-1.0, -1.0), (0.0, -1.0), (1.0, -1.0), (-1.0, 0.0), (1.0, 0.0), (-1.0, 1.0), (0.0, 1.0), (1.0, 1.0)] {
            target.DrawTextLayout(Vector2 { X: area.left + dx, Y: area.top + dy }, &layout, &outline, D2D1_DRAW_TEXT_OPTIONS_CLIP);
        }
        target.DrawTextLayout(Vector2 { X: area.left, Y: area.top }, &layout, &foreground, D2D1_DRAW_TEXT_OPTIONS_CLIP);
    }
    Ok(())
}

fn paint(state: &Inventory) -> Result<Vec<u8>> {
    render(state.size(), state.scale, |target, imaging, write| unsafe {
        target.Clear(Some(&color(0x2b2626)));
        fill(target, rect(4.0, 4.0, 428.0, HEIGHT - 4.0), if state.drop_hover { 0xe3bd79 } else { 0x59463c })?;
        fill(target, rect(8.0, 8.0, 424.0, HEIGHT - 8.0), 0x302b2b)?;
        fill(target, rect(12.0, 12.0, 420.0, 78.0), 0x3e3532)?;
        fill(target, rect(12.0, 75.0, 420.0, 79.0), 0x7e6650)?;
        text(target, write, "VelvetFrost  /", rect(30.0, 17.0, 250.0, 37.0), 10.0, 0xb9a794, false, true)?;
        text(target, write, tr("collection"), rect(30.0, 35.0, 335.0, 68.0), 22.0, 0xf2e6d5, false, true)?;
        text(target, write, "×", rect(378.0, 24.0, 411.0, 63.0), 24.0, 0xe1cbb1, true, false)?;
        for (selected, left, label) in [(state.stored, 40.0, "collection.tab_stored"), (!state.stored, 218.0, "collection.tab_placed")] {
            fill(target, rect(left, 93.0, left + 174.0, 127.0), if selected { 0x947656 } else { 0x49403a })?;
            fill(target, rect(left + 3.0, 96.0, left + 171.0, 124.0), if selected { 0x66513f } else { 0x36302f })?;
            text(
                target,
                write,
                tr(label),
                rect(left + 4.0, 96.0, left + 170.0, 124.0),
                12.0,
                if selected { 0xf4e5ce } else { 0xaea095 },
                true,
                selected,
            )?;
        }
        let hovered = state.hover.and_then(|point| state.hit(point));
        for category in Category::ALL {
            let filtered = state.filtered(category);
            let offset = state.offsets[category.index()];
            let y = category.top();
            let height = category.height();
            text(target, write, tr(category.label()), rect(LEFT, y - 23.0, WIDTH - LEFT, y - 2.0), 12.0, 0xd6c5b1, false, true)?;
            if !filtered.is_empty() {
                let count = format!("{}–{} / {}", offset + 1, (offset + COLS).min(filtered.len()), filtered.len());
                let right = if category == Category::Trees { 306.0 } else { 392.0 };
                text(target, write, &count, rect(right - 80.0, y - 23.0, right, y - 2.0), 10.0, 0xb9a794, true, false)?;
            }
            if category == Category::Trees {
                for (feet, left) in [(false, 312.0), (true, 352.0)] {
                    let selected = feet == state.height_in_feet;
                    fill(target, rect(left, 135.0, left + 38.0, 155.0), if selected { 0x80674f } else { 0x49403a })?;
                    text(
                        target,
                        write,
                        tr(if feet { "collection.unit_ft" } else { "collection.unit_m" }),
                        rect(left, 135.0, left + 38.0, 155.0),
                        10.0,
                        if selected { 0xf4e5ce } else { 0xaea095 },
                        true,
                        selected,
                    )?;
                }
            }
            let arrow_y = y + (height - 32.0) / 2.0;
            text(target, write, "‹", rect(12.0, arrow_y, 38.0, arrow_y + 32.0), 24.0, if offset > 0 { 0xf3dfc2 } else { 0x66584e }, true, true)?;
            text(
                target,
                write,
                "›",
                rect(394.0, arrow_y, 420.0, arrow_y + 32.0),
                24.0,
                if offset + COLS < filtered.len() { 0xf3dfc2 } else { 0x66584e },
                true,
                true,
            )?;
            for slot in 0..COLS {
                let x = LEFT + slot as f32 * SLOT;
                let position = offset + slot;
                let focused = state.keyboard && state.focus_category == category && position == state.focus && position < filtered.len();
                let hovered = hovered.is_some() && filtered.get(position).copied() == hovered;
                fill(
                    target,
                    rect(x, y, x + 78.0, y + height),
                    if focused {
                        0xe3bd79
                    } else if hovered {
                        0x8b7059
                    } else {
                        0x655347
                    },
                )?;
                fill(target, rect(x + 3.0, y + 3.0, x + 75.0, y + height - 3.0), if hovered { 0x30302f } else { 0x252728 })?;
                fill(target, rect(x + 6.0, y + 6.0, x + 72.0, y + height - 6.0), if hovered { 0x3b3b39 } else { 0x303333 })?;
                if let Some(index) = filtered.get(position) {
                    let item = &state.entries[*index];
                    let extra = if item.tree_height_mm.is_some() { 14.0 } else { 0.0 };
                    image(target, imaging, item.object, rect(x + 8.0, y + 4.0, x + 70.0, y + height - 4.0 - extra))?;
                    outlined_label(target, write, name(item.object), rect(x + 4.0, y + height - 31.0 - extra, x + 74.0, y + height - 4.0 - extra))?;
                    if let Some(mm) = item.tree_height_mm {
                        text(
                            target,
                            write,
                            &height_label(mm, state.height_in_feet),
                            rect(x + 3.0, y + height - 16.0, x + 75.0, y + height - 2.0),
                            10.0,
                            0xd5bc93,
                            true,
                            true,
                        )?;
                    }
                }
            }
            if filtered.is_empty() {
                fill(target, rect(LEFT, y, WIDTH - LEFT, y + height), 0x655347)?;
                fill(target, rect(LEFT + 3.0, y + 3.0, WIDTH - LEFT - 3.0, y + height - 3.0), 0x303333)?;
                let key = match (category, state.stored) {
                    (Category::Trees, true) => "collection.empty_trees_stored",
                    (Category::Trees, false) => "collection.empty_trees_placed",
                    (Category::Toys, true) => "collection.empty_toys_stored",
                    (Category::Toys, false) => "collection.empty_toys_placed",
                };
                let center = y + height / 2.0;
                text(target, write, tr(key), rect(50.0, center - 18.0, 382.0, center), 11.0, 0xe5d7c5, true, false)?;
                text(
                    target,
                    write,
                    tr(if state.stored { "collection.empty_hint_stored" } else { "collection.empty_hint_placed" }),
                    rect(50.0, center, 382.0, center + 23.0),
                    9.5,
                    0xc5b5a2,
                    true,
                    false,
                )?;
            }
        }
        let selected = hovered.or_else(|| state.keyboard.then(|| state.filtered(state.focus_category).get(state.focus).copied()).flatten());
        if let Some(item) = selected.and_then(|index| state.entries.get(index)) {
            text(target, write, name(item.object), rect(24.0, 387.0, 408.0, 405.0), 11.0, 0xf1dfc9, false, true)?;
            text(target, write, &monitor_label(&item.monitor), rect(24.0, 404.0, 408.0, 422.0), 10.5, 0xd1c1aa, false, false)?;
        }
        text(
            target,
            write,
            tr(if state.drop_hover {
                "collection.drop_hint"
            } else if state.stored {
                "collection.drag_hint"
            } else {
                "collection.store_hint"
            }),
            rect(24.0, 422.0, 408.0, 441.0),
            10.5,
            0xd1c1aa,
            false,
            false,
        )?;
        if let Some(message) = &state.message {
            text(
                target,
                write,
                message,
                rect(24.0, 444.0, if state.undo_display.is_some() { 313.0 } else { 408.0 }, 472.0),
                10.5,
                0xf1dfc9,
                false,
                false,
            )?;
        }
        if state.undo_display.is_some() {
            fill(target, rect(320.0, 445.0, 408.0, 470.0), 0x80674f)?;
            text(target, write, tr("collection.undo"), rect(320.0, 445.0, 408.0, 470.0), 10.0, 0xf4e5ce, true, true)?;
        }
        text(target, write, tr("collection.keyboard_help"), rect(20.0, 483.0, 412.0, 504.0), 10.5, 0xcbb9a6, true, false)?;
        Ok(())
    })
}

fn monitor_label(id: &str) -> String {
    let short = id.rsplit('\\').next().unwrap_or(id);
    let label = tr("collection.monitor").replace("{id}", short);
    let wide: Vec<u16> = id.encode_utf16().chain([0]).collect();
    let mut display = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
    if unsafe { EnumDisplayDevicesW(PCWSTR(wide.as_ptr()), 0, &mut display, 0) }.as_bool() {
        let end = display.DeviceString.iter().position(|&unit| unit == 0).unwrap_or(display.DeviceString.len());
        let name = String::from_utf16_lossy(&display.DeviceString[..end]);
        if !name.trim().is_empty() && !name.eq_ignore_ascii_case(short) {
            return format!("{label} · {name}");
        }
    }
    label
}

fn height_value(mm: u32, feet: bool) -> f64 {
    if feet { mm as f64 / 304.8 } else { mm as f64 / 1000.0 }
}

fn height_label(mm: u32, feet: bool) -> String {
    let value = height_value(mm, feet);
    let number = format!("{value:.2}");
    let number = if matches!(snowhush_core::text::code(), "de" | "es" | "fr" | "pt" | "ru" | "uk") { number.replace('.', ",") } else { number };
    format!("{} {}", number, tr(if feet { "collection.unit_ft" } else { "collection.unit_m" }))
}

fn name(object: CollectionObject) -> &'static str {
    tr(match object {
        CollectionObject::Tree(_, _, crate::sculpt::CELESTIAL_VARIANT) => "collection.celestial",
        CollectionObject::Tree(_, _, crate::sculpt::RUBY_VARIANT) => "collection.ruby",
        CollectionObject::Tree(_, _, crate::sculpt::ROSE_VARIANT) => "collection.rose",
        CollectionObject::Tree(_, Species::Spruce, _) => "collection.spruce",
        CollectionObject::Tree(_, Species::Pine, _) => "collection.pine",
        CollectionObject::Tree(_, Species::Fir, _) => "collection.fir",
        CollectionObject::Gift(GiftKind::Lantern) => "collection.lantern",
        CollectionObject::Gift(GiftKind::Boombox) => "collection.boombox",
    })
}

fn asset(object: CollectionObject) -> &'static [u8] {
    match object {
        CollectionObject::Tree(_, _, crate::sculpt::CELESTIAL_VARIANT) => include_bytes!("../assets/pixel-celestial-tree.png"),
        CollectionObject::Tree(_, _, crate::sculpt::RUBY_VARIANT) => include_bytes!("../assets/pixel-ruby-tree-v2.png"),
        CollectionObject::Tree(_, _, crate::sculpt::ROSE_VARIANT) => include_bytes!("../assets/pixel-rose-tree.png"),
        CollectionObject::Tree(_, Species::Spruce, 1) => include_bytes!("../assets/pixel-spruce-1.png"),
        CollectionObject::Tree(_, Species::Spruce, 2) => include_bytes!("../assets/pixel-spruce-2.png"),
        CollectionObject::Tree(_, Species::Spruce, 3) => include_bytes!("../assets/pixel-spruce-3.png"),
        CollectionObject::Tree(_, Species::Spruce, _) => include_bytes!("../assets/pixel-spruce-0.png"),
        CollectionObject::Tree(_, Species::Pine, 1) => include_bytes!("../assets/pixel-pine-1.png"),
        CollectionObject::Tree(_, Species::Pine, 2) => include_bytes!("../assets/pixel-pine-2.png"),
        CollectionObject::Tree(_, Species::Pine, 3) => include_bytes!("../assets/pixel-pine-3.png"),
        CollectionObject::Tree(_, Species::Pine, _) => include_bytes!("../assets/pixel-pine-0.png"),
        CollectionObject::Tree(_, Species::Fir, 1) => include_bytes!("../assets/pixel-fir-1.png"),
        CollectionObject::Tree(_, Species::Fir, 2) => include_bytes!("../assets/pixel-fir-2.png"),
        CollectionObject::Tree(_, Species::Fir, 3) => include_bytes!("../assets/pixel-fir-3.png"),
        CollectionObject::Tree(_, Species::Fir, _) => include_bytes!("../assets/pixel-fir-0.png"),
        CollectionObject::Gift(GiftKind::Lantern) => include_bytes!("../assets/pixel-warm-lantern.png"),
        CollectionObject::Gift(GiftKind::Boombox) => include_bytes!("../assets/pixel-boombox.png"),
    }
}

fn image(target: &ID2D1RenderTarget, imaging: &IWICImagingFactory, object: CollectionObject, bounds: D2D_RECT_F) -> Result<()> {
    unsafe {
        let stream = SHCreateMemStream(Some(asset(object))).ok_or_else(Error::empty)?;
        let decoder = imaging.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let converter = imaging.CreateFormatConverter()?;
        converter.Initialize(
            &decoder.GetFrame(0)?,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let bitmap = target.CreateBitmapFromWicBitmap(&converter, None)?;
        let size = bitmap.GetSize();
        let scale = ((bounds.right - bounds.left) / size.width).min((bounds.bottom - bounds.top) / size.height);
        let width = size.width * scale;
        let height = size.height * scale;
        let left = (bounds.left + bounds.right - width) * 0.5;
        let top = (bounds.top + bounds.bottom - height) * 0.5;
        target.DrawBitmap(&bitmap, Some(&rect(left, top, left + width, top + height)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
    }
    Ok(())
}

/// The preview is a separate feature and keeps its sample collection entirely in memory.
#[cfg(feature = "inventory-preview")]
pub fn preview() {
    use std::rc::Rc;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let entries = Rc::new(RefCell::new(vec![
        CollectionEntry {
            monitor: "DISPLAY1".into(),
            object: CollectionObject::Tree(1, Species::Spruce, 0),
            stored: true,
            tree_height_mm: Some(1350),
        },
        CollectionEntry { monitor: "DISPLAY1".into(), object: CollectionObject::Tree(2, Species::Pine, 1), stored: true, tree_height_mm: Some(1940) },
        CollectionEntry { monitor: "DISPLAY1".into(), object: CollectionObject::Gift(GiftKind::Lantern), stored: true, tree_height_mm: None },
        CollectionEntry { monitor: "DISPLAY1".into(), object: CollectionObject::Gift(GiftKind::Boombox), stored: true, tree_height_mm: None },
        CollectionEntry { monitor: "DISPLAY1".into(), object: CollectionObject::Tree(3, Species::Fir, 2), stored: false, tree_height_mm: Some(1680) },
        CollectionEntry {
            monitor: "DISPLAY2".into(),
            object: CollectionObject::Tree(4, Species::Spruce, 0),
            stored: true,
            tree_height_mm: Some(1710),
        },
        CollectionEntry { monitor: "DISPLAY2".into(), object: CollectionObject::Tree(5, Species::Pine, 1), stored: true, tree_height_mm: Some(1550) },
        CollectionEntry { monitor: "DISPLAY2".into(), object: CollectionObject::Tree(6, Species::Fir, 2), stored: true, tree_height_mm: Some(1810) },
        CollectionEntry { monitor: "DISPLAY2".into(), object: CollectionObject::Gift(GiftKind::Lantern), stored: true, tree_height_mm: None },
        CollectionEntry { monitor: "DISPLAY2".into(), object: CollectionObject::Gift(GiftKind::Boombox), stored: true, tree_height_mm: None },
        CollectionEntry { monitor: "DISPLAY3".into(), object: CollectionObject::Gift(GiftKind::Lantern), stored: true, tree_height_mm: None },
    ]));
    let snapshot = entries.clone();
    let store = entries.clone();
    let undone = entries.clone();
    let last = Rc::new(RefCell::new(None::<CollectionEntry>));
    let stored_last = last.clone();
    let place = entries.clone();
    let handlers = Handlers {
        snapshot: Box::new(move || snapshot.borrow().clone()),
        height_in_feet: false,
        set_height_in_feet: Box::new(|_| {}),
        store: Box::new(move |item| {
            if let Some(entry) = store.borrow_mut().iter_mut().find(|entry| entry.object == item.object && entry.monitor == item.monitor) {
                entry.stored = !item.stored;
                *stored_last.borrow_mut() = (!item.stored).then_some(item);
                Ok(0)
            } else {
                Err(CollectionError::Unavailable)
            }
        }),
        undo: Box::new(move |_| {
            let item = last.borrow_mut().take().ok_or(CollectionError::Unavailable)?;
            let mut entries = undone.borrow_mut();
            let entry = entries
                .iter_mut()
                .find(|entry| entry.object == item.object && entry.monitor == item.monitor && entry.stored)
                .ok_or(CollectionError::Unavailable)?;
            entry.stored = false;
            Ok(())
        }),
        place: Box::new(move |item, _| {
            if let Some(entry) = place.borrow_mut().iter_mut().find(|entry| entry.object == item.object && entry.monitor == item.monitor) {
                entry.stored = false;
                Ok(())
            } else {
                Err(CollectionError::Unavailable)
            }
        }),
    };
    if show(handlers).is_err() {
        return;
    }
    unsafe {
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
            if INVENTORY.with_borrow(|state| state.is_none()) {
                break;
            }
        }
    }
}
