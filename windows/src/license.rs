//! The exact upstream notice and license, available offline from the installer.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{DEFAULT_GUI_FONT, GetStockObject};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, Result, w};

pub const TEXT: &str = include_str!("../../LICENSE");
pub const THIRD_PARTY: &str = include_str!("../../THIRD-PARTY-NOTICES.txt");
pub const RUST_HTML: &str = include_str!("../../notices/RUST-STANDARD-LIBRARY-NOTICES.html");
const RUST_TEXT: &str = include_str!(concat!(env!("OUT_DIR"), "/rust-library-notices.txt"));

pub fn all_text() -> &'static str {
    static ALL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ALL.get_or_init(|| format!("{TEXT}\n\n================ THIRD-PARTY NOTICES ================\n\n{THIRD_PARTY}\n\n================ RUST STANDARD LIBRARY ================\n\n{RUST_TEXT}"))
}
const CLASS: PCWSTR = w!("VelvetFrostLicense");
const EDIT_ID: i32 = 1;

/// Show a selectable, scrollable copy without requiring a network or a temporary file.
pub fn show(owner: Option<HWND>) -> Result<HWND> {
    crate::card::register(CLASS, Some(procedure))?;
    unsafe {
        let title = HSTRING::from(snowhush_core::text::tr("setup.license"));
        let window = CreateWindowExW(
            WS_EX_APPWINDOW,
            CLASS,
            &title,
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            720,
            620,
            owner,
            None,
            None,
            None,
        )?;
        let text = HSTRING::from(all_text().replace("\r\n", "\n").replace('\n', "\r\n"));
        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_TABSTOP | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
            0,
            0,
            0,
            0,
            Some(window),
            Some(HMENU(EDIT_ID as isize as *mut _)),
            None,
            None,
        )?;
        if SendMessageW(edit, WM_SETTEXT, None, Some(LPARAM(text.as_ptr() as isize))).0 == 0 {
            let _ = DestroyWindow(window);
            return Err(windows::core::Error::new(windows::core::HRESULT(0x80004005u32 as i32), "Unable to display license notices"));
        }
        let font = GetStockObject(DEFAULT_GUI_FONT);
        SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        resize(window);
        let _ = SetForegroundWindow(window);
        Ok(window)
    }
}

fn resize(window: HWND) {
    unsafe {
        if let Ok(edit) = GetDlgItem(Some(window), EDIT_ID) {
            let mut rect = Default::default();
            if GetClientRect(window, &mut rect).is_ok() {
                let _ = MoveWindow(edit, 0, 0, rect.right, rect.bottom, true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an interactive Windows desktop"]
    fn viewer_displays_the_entire_embedded_license() {
        unsafe {
            let owner =
                CreateWindowExW(WINDOW_EX_STYLE::default(), w!("STATIC"), w!(""), WS_OVERLAPPED, 0, 0, 100, 100, None, None, None, None).unwrap();
            let viewer = show(Some(owner)).unwrap();
            let edit = GetDlgItem(Some(viewer), EDIT_ID).unwrap();
            let mut buffer = vec![0u16; GetWindowTextLengthW(edit) as usize + 1];
            let len = GetWindowTextW(edit, &mut buffer) as usize;
            let shown = String::from_utf16(&buffer[..len]).unwrap();
            assert_eq!(shown.replace("\r\n", "\n"), all_text().replace("\r\n", "\n"));
            let _ = DestroyWindow(viewer);
            let _ = DestroyWindow(owner);
        }
    }
}

unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_SIZE => {
            resize(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            // A standalone --license window owns its own message loop.
            if unsafe { GetWindow(window, GW_OWNER) }.is_err() {
                unsafe { PostQuitMessage(0) };
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}
