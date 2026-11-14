#![windows_subsystem = "windows"]
#![cfg_attr(feature = "inventory-preview", allow(dead_code))]

mod app;
mod bullfinch;
mod cone;
mod desktop;
mod events;
mod fox;
mod gifts;
mod halloween;
mod inventory;
mod light;
mod log;
mod menu;
mod ornaments;
mod plow;
mod prefs;
mod render;
mod screen;
mod sculpt;
mod sleigh;
mod snow;
mod star_event;
mod tracker;
mod traffic;
mod tree_space;
mod truck;
mod update;
mod update_dialog;
mod welcome;

#[cfg(not(feature = "inventory-preview"))]
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

#[cfg(feature = "inventory-preview")]
fn main() {
    snowhush::language::choose();
    inventory::preview();
}

#[cfg(not(feature = "inventory-preview"))]
fn main() {
    snowhush::language::choose();
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.iter().any(|argument| argument.eq_ignore_ascii_case("--uninstall")) {
        let decision = uninstall_decision(&arguments, snowhush::install::confirm_uninstall);
        if let Some(delete_settings) = decision {
            snowhush::install::uninstall(delete_settings);
        }
        return;
    }
    let Some(_instance) = snowhush::instance::Instance::acquire() else { return };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    log::start();
    log!("VelvetFrost {}", env!("CARGO_PKG_VERSION"));
    if let Err(error) = app::run() {
        log!("failed: {error}");
    }
}

#[cfg(not(feature = "inventory-preview"))]
fn uninstall_decision(arguments: &[String], confirm: impl FnOnce() -> Option<bool>) -> Option<bool> {
    if arguments.iter().any(|argument| argument.eq_ignore_ascii_case("--silent") || argument.eq_ignore_ascii_case("/s")) {
        Some(false)
    } else {
        confirm()
    }
}

#[cfg(all(test, not(feature = "inventory-preview")))]
mod uninstall_tests {
    use super::uninstall_decision;

    #[test]
    fn silent_uninstall_keeps_data_and_interactive_uninstall_obeys_the_choice() {
        for flag in ["--silent", "/S"] {
            assert_eq!(uninstall_decision(&[flag.to_owned()], || panic!("silent uninstall must not show a dialog")), Some(false));
        }
        assert_eq!(uninstall_decision(&[], || None), None);
        assert_eq!(uninstall_decision(&[], || Some(false)), Some(false));
        assert_eq!(uninstall_decision(&[], || Some(true)), Some(true));
    }
}
