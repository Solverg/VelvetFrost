//! Confirmation card for a fully downloaded and verified update.

use std::cell::RefCell;
use std::sync::Once;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, Result, w};

use snowhush::card::{self, Card, Content, Hits};
use snowhush_core::text::{self, tr};

const CLASS: PCWSTR = w!("VelvetFrostUpdateConfirmation");
const WIDTH: f32 = 400.0;
const HEIGHT: f32 = 360.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Install,
    Later,
}

struct Dialog {
    window: HWND,
    controller: HWND,
    card: Card,
    hits: Hits,
    version: String,
    held: Option<Control>,
}

thread_local! { static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) }; }

pub fn show(controller: HWND, version: &str) -> Result<()> {
    if let Some(window) = DIALOG.with_borrow(|dialog| dialog.as_ref().map(|dialog| dialog.window)) {
        unsafe {
            let _ = SetForegroundWindow(window);
        }
        return Ok(());
    }
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        let _ = card::register(CLASS, Some(procedure));
    });
    let body = text::fill("update.card_body", &[("version", version)]);
    let content = contents(&body, false);
    let (scale, _) = card::placement(WIDTH, HEIGHT);
    let height = Card::new(WIDTH, HEIGHT, scale)?.fitting_height(&[content], HEIGHT)?;
    let (scale, origin) = card::placement(WIDTH, height);
    let card = Card::new(WIDTH, height, scale)?;
    let (pixels, hits) = card.paint(&contents(&body, false))?;
    unsafe {
        let window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!("VelvetFrost"),
            WS_POPUP,
            origin.x,
            origin.y,
            card.size.0,
            card.size.1,
            None,
            None,
            None,
            None,
        )?;
        card::present(window, card.size, &pixels, Some(origin));
        DIALOG.set(Some(Dialog { window, controller, card, hits, version: version.to_string(), held: None }));
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
    }
    Ok(())
}

fn contents(body: &str, pressed: bool) -> Content<'_> {
    Content {
        icon: 72.0,
        title: tr("update.card_title"),
        title_size: 28.0,
        body,
        button: tr("update.install_button"),
        pressed,
        closable: true,
        link: Some(tr("update.later")),
    }
}

impl Dialog {
    fn control(&self, lparam: LPARAM) -> Option<Control> {
        let point = self.card.dips(card::point(lparam));
        if card::contains(&self.hits.button, point) {
            Some(Control::Install)
        } else if self.hits.link.is_some_and(|rect| card::contains(&rect, point)) || self.hits.close.is_some_and(|rect| card::contains(&rect, point))
        {
            Some(Control::Later)
        } else {
            None
        }
    }

    fn repaint(&mut self, pressed: bool) {
        let body = text::fill("update.card_body", &[("version", &self.version)]);
        if let Ok((pixels, hits)) = self.card.paint(&contents(&body, pressed)) {
            self.hits = hits;
            card::present(self.window, self.card.size, &pixels, None);
        }
    }

    fn press_down(&mut self, lparam: LPARAM) -> bool {
        self.held = self.control(lparam);
        self.held == Some(Control::Install)
    }

    fn press_up(&mut self, lparam: LPARAM) -> Option<Control> {
        let clicked = self.held.filter(|held| Some(*held) == self.control(lparam));
        self.held = None;
        clicked
    }
}

unsafe extern "system" fn procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_LBUTTONDOWN => {
            DIALOG.with_borrow_mut(|dialog| {
                if let Some(dialog) = dialog {
                    let pressed = dialog.press_down(lparam);
                    dialog.repaint(pressed);
                }
            });
            unsafe {
                SetCapture(window);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            unsafe {
                let _ = ReleaseCapture();
            }
            let clicked = DIALOG.with_borrow_mut(|dialog| {
                let dialog = dialog.as_mut()?;
                let clicked = dialog.press_up(lparam);
                dialog.repaint(false);
                clicked
            });
            if let Some(clicked) = clicked {
                finish(clicked == Control::Install);
            }
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_RETURN.0 as usize => {
            finish(true);
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            finish(false);
            LRESULT(0)
        }
        WM_CLOSE => {
            finish(false);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn finish(install: bool) {
    if let Some(dialog) = DIALOG.take() {
        unsafe {
            let _ = DestroyWindow(dialog.window);
        }
        if install {
            crate::app::install_ready(dialog.controller, &dialog.version);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    #[test]
    fn later_close_and_cancelled_click_never_select_install() {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
        let body = text::fill("update.card_body", &[("version", "99.0.0")]);
        let height = Card::new(WIDTH, HEIGHT, 1.0).unwrap().fitting_height(&[contents(&body, false)], HEIGHT).unwrap();
        let card = Card::new(WIDTH, height, 1.0).unwrap();
        let (_, hits) = card.paint(&contents(&body, false)).unwrap();
        let mut dialog = Dialog { window: HWND::default(), controller: HWND::default(), card, hits, version: "99.0.0".into(), held: None };
        let point = |rect: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F| {
            let x = ((rect.left + rect.right) / 2.0) as u32;
            let y = ((rect.top + rect.bottom) / 2.0) as u32;
            LPARAM((x | (y << 16)) as isize)
        };
        let button = point(hits.button);
        let later = point(hits.link.unwrap());
        let close = point(hits.close.unwrap());
        assert!(!dialog.press_down(later));
        assert!(matches!(dialog.press_up(later), Some(Control::Later)));
        assert!(!dialog.press_down(close));
        assert!(matches!(dialog.press_up(close), Some(Control::Later)));
        assert!(dialog.press_down(button));
        assert!(dialog.press_up(later).is_none());
        assert!(dialog.press_down(button));
        assert!(matches!(dialog.press_up(button), Some(Control::Install)));
    }

    /// Renders the actual card without opening an app, accessing the registry,
    /// or offering an installation. The caller supplies a new preview path.
    #[test]
    #[ignore = "writes an explicitly requested offscreen preview"]
    fn render_update_card_offscreen() {
        snowhush_core::text::choose(&["ru"]);
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
        let body = text::fill("update.card_body", &[("version", "99.0.0")]);
        let height = Card::new(WIDTH, HEIGHT, 1.0).unwrap().fitting_height(&[contents(&body, false)], HEIGHT).unwrap();
        let card = Card::new(WIDTH, height, 1.0).unwrap();
        let (pixels, _) = card.paint(&contents(&body, false)).unwrap();
        let (width, height) = card.size;
        let size = 54 + width as usize * height as usize * 4;
        let mut bmp = Vec::with_capacity(size);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(size as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&width.to_le_bytes());
        bmp.extend_from_slice(&(-height).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 24]);
        for pixel in pixels.chunks_exact(4) {
            let background = 240u16 * (255 - pixel[3] as u16) / 255;
            bmp.extend_from_slice(&[
                (pixel[0] as u16 + background).min(255) as u8,
                (pixel[1] as u16 + background).min(255) as u8,
                (pixel[2] as u16 + background).min(255) as u8,
                255,
            ]);
        }
        let path = std::env::var_os("VELVETFROST_CARD_PREVIEW").expect("set a new preview file path");
        std::fs::OpenOptions::new().write(true).create_new(true).open(path).unwrap().write_all(&bmp).unwrap();
    }
}
