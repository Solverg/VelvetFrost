//! UI Automation exposes the custom-painted inventory as named, invokable controls.
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Error, IUnknown, IUnknownImpl, Interface, Result, implement};

pub const INVOKE: u32 = WM_APP + 41;
pub const FOCUS: u32 = WM_APP + 42;

#[derive(Clone, PartialEq)]
pub struct Node {
    pub id: u32,
    pub name: String,
    pub status: String,
    pub enabled: bool,
    pub bounds: [f64; 4],
}

#[derive(Default)]
pub struct Snapshot {
    pub nodes: Vec<Node>,
    pub focus: u32,
    pub window_bounds: [f64; 4],
    pub generation: u32,
    pub name: String,
}

pub type Shared = Arc<Mutex<Snapshot>>;

#[implement(IRawElementProviderSimple, IRawElementProviderFragment, IRawElementProviderFragmentRoot, IInvokeProvider)]
struct Provider {
    hwnd: isize,
    id: u32,
    shared: Shared,
}

fn provider(hwnd: isize, id: u32, shared: &Shared) -> Provider {
    Provider { hwnd, id, shared: shared.clone() }
}

pub fn root(window: HWND, shared: &Shared) -> IRawElementProviderSimple {
    provider(window.0 as isize, 0, shared).into()
}

pub fn announce_focus(window: HWND, shared: &Shared, id: u32) {
    if unsafe { UiaClientsAreListening() }.as_bool() {
        let element: IRawElementProviderSimple = provider(window.0 as isize, id, shared).into();
        unsafe {
            let _ = UiaRaiseAutomationEvent(&element, UIA_AutomationFocusChangedEventId);
        }
    }
}

fn unavailable() -> Error {
    Error::from_hresult(E_FAIL)
}

impl IRawElementProviderSimple_Impl for Provider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        if self.id == 0 || pattern != UIA_InvokePatternId {
            return Err(unavailable());
        }
        let invoke: IInvokeProvider = self.to_interface();
        invoke.cast()
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let snapshot = self.shared.lock().map_err(|_| unavailable())?;
        let node = snapshot.nodes.iter().find(|node| node.id == self.id);
        if self.id != 0 && node.is_none() {
            return Err(unavailable());
        }
        if property == UIA_NamePropertyId {
            let name = node.map_or(snapshot.name.as_str(), |node| node.name.as_str());
            Ok(VARIANT::from(name))
        } else if property == UIA_ControlTypePropertyId {
            Ok(VARIANT::from(if self.id == 0 { UIA_WindowControlTypeId.0 } else { UIA_ButtonControlTypeId.0 }))
        } else if property == UIA_IsEnabledPropertyId {
            Ok(VARIANT::from(node.is_none_or(|node| node.enabled)))
        } else if property == UIA_IsKeyboardFocusablePropertyId
            || property == UIA_IsControlElementPropertyId
            || property == UIA_IsContentElementPropertyId
        {
            Ok(VARIANT::from(true))
        } else if property == UIA_HasKeyboardFocusPropertyId {
            Ok(VARIANT::from(snapshot.focus == self.id))
        } else if property == UIA_ItemStatusPropertyId {
            Ok(VARIANT::from(node.map_or("", |node| node.status.as_str())))
        } else if property == UIA_SelectionItemIsSelectedPropertyId && (1..=4).contains(&self.id) {
            Ok(VARIANT::from(node.is_some_and(|node| !node.status.is_empty())))
        } else if property == UIA_AutomationIdPropertyId {
            Ok(VARIANT::from(format!("collection-{}", self.id).as_str()))
        } else {
            Ok(VARIANT::default())
        }
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        if self.id == 0 { unsafe { UiaHostProviderFromHwnd(HWND(self.hwnd as *mut _)) } } else { Err(unavailable()) }
    }
}

impl IRawElementProviderFragment_Impl for Provider_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        let snapshot = self.shared.lock().map_err(|_| unavailable())?;
        let id = if direction == NavigateDirection_Parent && self.id != 0 {
            Some(0)
        } else if self.id == 0 && direction == NavigateDirection_FirstChild {
            snapshot.nodes.first().map(|node| node.id)
        } else if self.id == 0 && direction == NavigateDirection_LastChild {
            snapshot.nodes.last().map(|node| node.id)
        } else if self.id != 0 {
            let position = snapshot.nodes.iter().position(|node| node.id == self.id);
            if direction == NavigateDirection_NextSibling {
                position.and_then(|i| snapshot.nodes.get(i + 1)).map(|node| node.id)
            } else if direction == NavigateDirection_PreviousSibling {
                position.and_then(|i| i.checked_sub(1)).and_then(|i| snapshot.nodes.get(i)).map(|node| node.id)
            } else {
                None
            }
        } else {
            None
        };
        id.map(|id| provider(self.hwnd, id, &self.shared).into()).ok_or_else(unavailable)
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        if self.id == 0 {
            return Ok(std::ptr::null_mut());
        }
        unsafe {
            let array = SafeArrayCreateVector(VT_I4, 0, 2);
            if array.is_null() {
                return Err(unavailable());
            }
            let parent = UiaAppendRuntimeId as i32;
            let id = self.id as i32;
            SafeArrayPutElement(array, &0, (&raw const parent).cast())?;
            SafeArrayPutElement(array, &1, (&raw const id).cast())?;
            Ok(array)
        }
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let snapshot = self.shared.lock().map_err(|_| unavailable())?;
        let bounds =
            if self.id == 0 { snapshot.window_bounds } else { snapshot.nodes.iter().find(|node| node.id == self.id).ok_or_else(unavailable)?.bounds };
        Ok(UiaRect { left: bounds[0], top: bounds[1], width: bounds[2], height: bounds[3] })
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }
    fn SetFocus(&self) -> Result<()> {
        self.post(FOCUS)
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(provider(self.hwnd, 0, &self.shared).into())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Provider_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let snapshot = self.shared.lock().map_err(|_| unavailable())?;
        let id = snapshot
            .nodes
            .iter()
            .find(|node| x >= node.bounds[0] && y >= node.bounds[1] && x < node.bounds[0] + node.bounds[2] && y < node.bounds[1] + node.bounds[3])
            .map_or(0, |node| node.id);
        Ok(provider(self.hwnd, id, &self.shared).into())
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        let snapshot = self.shared.lock().map_err(|_| unavailable())?;
        Ok(provider(self.hwnd, snapshot.focus, &self.shared).into())
    }
}

impl IInvokeProvider_Impl for Provider_Impl {
    fn Invoke(&self) -> Result<()> {
        self.post(INVOKE)
    }
}

impl Provider_Impl {
    fn post(&self, message: u32) -> Result<()> {
        let snapshot = self.shared.lock().map_err(|_| unavailable())?;
        let node = snapshot.nodes.iter().find(|node| node.id == self.id && node.enabled).ok_or_else(unavailable)?;
        let token = ((snapshot.generation as usize) << 32) | node.id as usize;
        unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), message, WPARAM(token), LPARAM(0)) }
    }
}
