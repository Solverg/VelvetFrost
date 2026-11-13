//! Installing for the current user and removing again, shared by the installer and the app's own uninstaller.
//! No administrator rights: everything lives under the user's profile and registry.

use std::ffi::c_void;
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::Win32::UI::Controls::{TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TaskDialogIndirect};
use windows::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_Programs, FOLDERID_UserProgramFiles, IShellLinkW, KF_FLAG_CREATE, SHGetKnownFolderPath, ShellExecuteW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowThreadProcessId, IDCANCEL, SMTO_ABORTIFHUNG, SW_SHOWNORMAL, SendMessageTimeoutW, WM_CLOSE,
};
use windows::core::{BOOL, GUID, HSTRING, Interface, PCWSTR, PWSTR, w};

const UNINSTALL: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\VelvetFrost");
const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const SETTINGS: PCWSTR = w!("Software\\VelvetFrost");
const NAME: PCWSTR = w!("VelvetFrost");
const CONTROLLER: PCWSTR = w!("VelvetFrostController");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The version recorded by the VelvetFrost installer.
pub fn installed_version() -> Option<String> {
    read_string(UNINSTALL, w!("DisplayVersion"))
}

/// Install or update VelvetFrost in its own per-user location.
pub fn install(app: &[u8], version: &str) -> Result<PathBuf, String> {
    let programs_folder = known_folder(&FOLDERID_UserProgramFiles).ok_or_else(|| snowhush_core::text::tr("install.folder").to_string())?;
    let folder = programs_folder.join("VelvetFrost");
    let exe = folder.join("VelvetFrost.exe");
    if !close_running(CONTROLLER, &exe) {
        return Err(snowhush_core::text::tr("install.busy").to_string());
    }
    // Prevent either application from starting during installation.
    let Some(_instance) = crate::instance::Instance::acquire() else { return Err(snowhush_core::text::tr("install.busy").to_string()) };
    finish_install(app, version, &programs_folder, known_folder(&FOLDERID_Programs).as_deref())
}

fn finish_install(app: &[u8], version: &str, programs_folder: &Path, shortcuts: Option<&Path>) -> Result<PathBuf, String> {
    let folder = programs_folder.join("VelvetFrost");
    let exe = folder.join("VelvetFrost.exe");
    std::fs::create_dir_all(&folder)
        .map_err(|error| snowhush_core::text::fill("install.create", &[("path", &folder.display().to_string()), ("error", &error.to_string())]))?;
    // All notices must be present before replacing the application.
    for (name, contents) in [
        ("LICENSE.txt", crate::license::TEXT),
        ("THIRD-PARTY-NOTICES.txt", crate::license::THIRD_PARTY),
        ("RUST-STANDARD-LIBRARY-NOTICES.html", crate::license::RUST_HTML),
    ] {
        let target = folder.join(name);
        let staged = folder.join(format!("{name}.new"));
        std::fs::write(&staged, contents)
            .map_err(|error| snowhush_core::text::fill("install.write", &[("path", &staged.display().to_string()), ("error", &error.to_string())]))?;
        if let Err(error) = std::fs::rename(&staged, &target) {
            let _ = std::fs::remove_file(&staged);
            return Err(snowhush_core::text::fill("install.replace", &[("path", &target.display().to_string()), ("error", &error.to_string())]));
        }
    }
    let staged = folder.join("VelvetFrost.exe.new");
    std::fs::write(&staged, app)
        .map_err(|error| snowhush_core::text::fill("install.write", &[("path", &staged.display().to_string()), ("error", &error.to_string())]))?;
    // A copy that was just asked to quit may hold on to its file for a moment longer.
    let started = Instant::now();
    while let Err(error) = std::fs::rename(&staged, &exe) {
        if started.elapsed() > Duration::from_secs(5) {
            let _ = std::fs::remove_file(&staged);
            return Err(snowhush_core::text::fill("install.replace", &[("path", &exe.display().to_string()), ("error", &error.to_string())]));
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let quoted = format!("\"{}\"", exe.display());
    for (name, value) in [
        (w!("DisplayName"), "VelvetFrost".to_string()),
        (w!("DisplayVersion"), version.to_string()),
        (w!("Publisher"), "VelvetFrost".to_string()),
        (w!("DisplayIcon"), exe.display().to_string()),
        (w!("InstallLocation"), folder.display().to_string()),
        (w!("UninstallString"), format!("{quoted} --uninstall")),
        (w!("QuietUninstallString"), format!("{quoted} --uninstall --silent")),
    ] {
        write_string(UNINSTALL, name, &value)?;
    }
    write_number(UNINSTALL, w!("NoModify"), 1)?;
    write_number(UNINSTALL, w!("NoRepair"), 1)?;
    write_number(UNINSTALL, w!("EstimatedSize"), app.len().div_ceil(1024) as u32)?;
    if let Some(programs) = shortcuts {
        let path = programs.join("VelvetFrost.lnk");
        shortcut(&exe, &path)
            .map_err(|error| snowhush_core::text::fill("install.write", &[("path", &path.display().to_string()), ("error", &error.to_string())]))?;
    }
    Ok(exe)
}

/// Ask before uninstalling; the optional data removal starts unchecked.
pub fn confirm_uninstall() -> Option<bool> {
    const REMOVE: i32 = 100;
    let title = HSTRING::from("VelvetFrost");
    let instruction = HSTRING::from(snowhush_core::text::tr("uninstall.title"));
    let body = HSTRING::from(snowhush_core::text::tr("uninstall.body"));
    let checkbox = HSTRING::from(snowhush_core::text::tr("uninstall.delete_data"));
    let remove = HSTRING::from(snowhush_core::text::tr("uninstall.remove"));
    let button = TASKDIALOG_BUTTON { nButtonID: REMOVE, pszButtonText: PCWSTR(remove.as_ptr()) };
    let config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(body.as_ptr()),
        cButtons: 1,
        pButtons: &button,
        nDefaultButton: IDCANCEL.0,
        pszVerificationText: PCWSTR(checkbox.as_ptr()),
        ..Default::default()
    };
    let mut selected = 0;
    let mut checked = BOOL(0);
    unsafe { TaskDialogIndirect(&config, Some(&mut selected), None, Some(&mut checked)).ok()? };
    (selected == REMOVE).then_some(checked.as_bool())
}

/// Remove VelvetFrost, preserving settings and collection unless requested.
pub fn uninstall(delete_settings: bool) {
    let Some(folder) = known_folder(&FOLDERID_UserProgramFiles).map(|path| path.join("VelvetFrost")) else { return };
    if !close_running(CONTROLLER, &folder.join("VelvetFrost.exe")) {
        return;
    }
    if let Some(programs) = known_folder(&FOLDERID_Programs) {
        let _ = std::fs::remove_file(programs.join("VelvetFrost.lnk"));
    }
    if let Some(data) = known_folder(&FOLDERID_LocalAppData) {
        let folder = data.join("VelvetFrost");
        let _ = std::fs::remove_file(folder.join("velvetfrost.log"));
        let _ = std::fs::remove_dir(folder);
    }
    remove_registration(delete_settings);
    let _ = schedule_cleanup(&folder);
}

fn schedule_cleanup(folder: &Path) -> std::io::Result<std::process::Child> {
    // A later reinstall may replace the executable before this delayed helper runs.
    // Only delete the file whose identity was observed during this uninstall.
    let metadata = std::fs::metadata(folder.join("VelvetFrost.exe"))?;
    std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", "Start-Sleep -Seconds 2; $p = Join-Path $env:VELVETFROST_CLEANUP_DIR 'VelvetFrost.exe'; $f = Get-Item -LiteralPath $p -ErrorAction SilentlyContinue; if ($null -ne $f -and $f.CreationTimeUtc.ToFileTimeUtc() -eq [long]$env:VELVETFROST_CLEANUP_CREATED -and $f.LastWriteTimeUtc.ToFileTimeUtc() -eq [long]$env:VELVETFROST_CLEANUP_WRITTEN -and $f.Length -eq [long]$env:VELVETFROST_CLEANUP_SIZE) { foreach ($name in @('LICENSE.txt', 'THIRD-PARTY-NOTICES.txt', 'RUST-STANDARD-LIBRARY-NOTICES.html')) { Remove-Item -LiteralPath (Join-Path $env:VELVETFROST_CLEANUP_DIR $name) -Force -ErrorAction SilentlyContinue }; Remove-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue; Remove-Item -LiteralPath $env:VELVETFROST_CLEANUP_DIR -ErrorAction SilentlyContinue }"])
            .env("VELVETFROST_CLEANUP_DIR", folder)
            .env("VELVETFROST_CLEANUP_CREATED", metadata.creation_time().to_string())
            .env("VELVETFROST_CLEANUP_WRITTEN", metadata.last_write_time().to_string())
            .env("VELVETFROST_CLEANUP_SIZE", metadata.file_size().to_string())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
}

fn remove_registration(delete_settings: bool) {
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL);
        if delete_settings {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, SETTINGS);
        }
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, NAME);
    }
}

pub fn launch(exe: &Path) {
    let path: Vec<u16> = exe.as_os_str().encode_wide_null();
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Only close a window if it is the executable we intend to replace.
fn close_running(class: PCWSTR, expected: &Path) -> bool {
    unsafe {
        let Ok(window) = FindWindowW(class, PCWSTR::null()) else { return true };
        let mut process = 0;
        GetWindowThreadProcessId(window, Some(&mut process));
        let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, false, process) else { return false };
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        let matches = QueryFullProcessImageNameW(handle, PROCESS_NAME_FORMAT(0), PWSTR(path.as_mut_ptr()), &mut length).is_ok()
            && paths_match(&String::from_utf16_lossy(&path[..length as usize]), expected);
        if !matches {
            let _ = CloseHandle(handle);
            return false;
        }
        let _ = SendMessageTimeoutW(window, WM_CLOSE, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 5000, None);
        let exited = WaitForSingleObject(handle, 5000) == WAIT_OBJECT_0;
        let _ = CloseHandle(handle);
        exited
    }
}

fn paths_match(actual: &str, expected: &Path) -> bool {
    actual.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(expected.to_string_lossy().trim_end_matches(['\\', '/']))
}

fn shortcut(target: &Path, at: &Path) -> windows::core::Result<()> {
    let target_path = target.as_os_str().encode_wide_null();
    let folder = target.parent().unwrap_or(target).as_os_str().encode_wide_null();
    let at = at.as_os_str().encode_wide_null();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(target_path.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(folder.as_ptr()))?;
        link.SetDescription(&HSTRING::from(snowhush_core::text::tr("shortcut")))?;
        link.cast::<IPersistFile>()?.Save(PCWSTR(at.as_ptr()), true)
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_CREATE, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const c_void));
        text.map(PathBuf::from)
    }
}

fn read_string(key: PCWSTR, name: PCWSTR) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, key, name, RRF_RT_REG_SZ, None, Some(buffer.as_mut_ptr().cast()), Some(&mut size)) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = buffer.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn write_string(key: PCWSTR, name: PCWSTR, value: &str) -> Result<(), String> {
    let value: Vec<u16> = value.encode_utf16().chain([0]).collect();
    registry_result(unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_SZ.0, Some(value.as_ptr().cast()), (value.len() * 2) as u32) })
}

fn write_number(key: PCWSTR, name: PCWSTR, value: u32) -> Result<(), String> {
    registry_result(unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_DWORD.0, Some((&raw const value).cast()), 4) })
}

fn registry_result(status: windows::Win32::Foundation::WIN32_ERROR) -> Result<(), String> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(snowhush_core::text::fill("install.write", &[("path", "Windows Registry"), ("error", &status.to_hresult().message().to_string())]))
    }
}

trait Wide {
    fn encode_wide_null(&self) -> Vec<u16>;
}

impl Wide for std::ffi::OsStr {
    fn encode_wide_null(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain([0]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::System::Registry::{
        HKEY, KEY_ALL_ACCESS, REG_OPTION_NON_VOLATILE, RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegOverridePredefKey,
    };

    #[test]
    fn install_update_and_uninstall_preserve_unrelated_snowhush_data() {
        const CHILD: &str = "VELVETFROST_INSTALL_TEST_ROOT";
        const FOLDER: &str = "VELVETFROST_INSTALL_TEST_FOLDER";
        const OTHER_SETTINGS: PCWSTR = w!("Software\\Snowhush");
        const OTHER_UNINSTALL: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Snowhush");
        const OTHER_STARTUP: PCWSTR = w!("Snowhush");
        if let (Ok(root), Some(folder)) = (std::env::var(CHILD), std::env::var_os(FOLDER)) {
            let root: Vec<u16> = root.encode_utf16().chain([0]).collect();
            let mut key = HKEY::default();
            unsafe {
                assert_eq!(RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(root.as_ptr()), None, KEY_ALL_ACCESS, &mut key), ERROR_SUCCESS);
                assert_eq!(RegOverridePredefKey(HKEY_CURRENT_USER, Some(key)), ERROR_SUCCESS);
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let folder = PathBuf::from(folder);
            let other_exe = folder.join("Snowhush").join("Snowhush.exe");
            std::fs::create_dir_all(other_exe.parent().unwrap()).unwrap();
            std::fs::write(&other_exe, b"unrelated app").unwrap();
            let other_command = format!("\"{}\"", other_exe.display());
            write_string(OTHER_SETTINGS, w!("collection"), "snowhush data").unwrap();
            write_string(OTHER_UNINSTALL, w!("DisplayName"), "Snowhush").unwrap();
            write_string(RUN, OTHER_STARTUP, &other_command).unwrap();
            write_string(RUN, NAME, "velvetfrost startup").unwrap();
            write_string(SETTINGS, w!("collection"), "velvetfrost data").unwrap();
            let app_folder = folder.join("VelvetFrost");
            std::fs::create_dir_all(app_folder.join("LICENSE.txt")).unwrap();
            std::fs::write(app_folder.join("VelvetFrost.exe"), b"previous").unwrap();
            assert!(finish_install(b"new", "1.0.0", &folder, None).is_err());
            assert_eq!(std::fs::read(app_folder.join("VelvetFrost.exe")).unwrap(), b"previous");
            assert!(!app_folder.join("LICENSE.txt.new").exists());
            std::fs::remove_dir(app_folder.join("LICENSE.txt")).unwrap();
            for name in ["THIRD-PARTY-NOTICES.txt", "RUST-STANDARD-LIBRARY-NOTICES.html"] {
                std::fs::create_dir(app_folder.join(name)).unwrap();
                assert!(finish_install(b"new", "1.0.0", &folder, None).is_err());
                assert_eq!(std::fs::read(app_folder.join("VelvetFrost.exe")).unwrap(), b"previous");
                assert!(!app_folder.join(format!("{name}.new")).exists());
                std::fs::remove_dir(app_folder.join(name)).unwrap();
            }
            let exe = finish_install(b"new", "1.0.0", &folder, None).unwrap();
            assert_eq!(std::fs::read(&exe).unwrap(), b"new");
            assert_eq!(std::fs::read_to_string(exe.parent().unwrap().join("LICENSE.txt")).unwrap(), crate::license::TEXT);
            assert_eq!(std::fs::read_to_string(app_folder.join("THIRD-PARTY-NOTICES.txt")).unwrap(), crate::license::THIRD_PARTY);
            assert_eq!(std::fs::read_to_string(app_folder.join("RUST-STANDARD-LIBRARY-NOTICES.html")).unwrap(), crate::license::RUST_HTML);
            assert_eq!(installed_version().as_deref(), Some("1.0.0"));
            assert_eq!(read_string(SETTINGS, w!("collection")).as_deref(), Some("velvetfrost data"));
            finish_install(b"updated", "1.0.1", &folder, None).unwrap();
            assert_eq!(std::fs::read(&exe).unwrap(), b"updated");
            assert_eq!(std::fs::read_to_string(exe.parent().unwrap().join("LICENSE.txt")).unwrap(), crate::license::TEXT);
            assert_eq!(installed_version().as_deref(), Some("1.0.1"));
            assert_eq!(read_string(SETTINGS, w!("collection")).as_deref(), Some("velvetfrost data"));
            assert_eq!(read_string(OTHER_SETTINGS, w!("collection")).as_deref(), Some("snowhush data"));
            assert_eq!(read_string(OTHER_UNINSTALL, w!("DisplayName")).as_deref(), Some("Snowhush"));
            assert_eq!(read_string(RUN, OTHER_STARTUP).as_deref(), Some(other_command.as_str()));
            assert_eq!(std::fs::read(&other_exe).unwrap(), b"unrelated app");
            remove_registration(false);
            assert!(read_string(UNINSTALL, w!("DisplayName")).is_none());
            assert_eq!(read_string(SETTINGS, w!("collection")).as_deref(), Some("velvetfrost data"));
            assert!(read_string(RUN, NAME).is_none());
            finish_install(b"reinstalled", "1.0.1", &folder, None).unwrap();
            assert_eq!(read_string(SETTINGS, w!("collection")).as_deref(), Some("velvetfrost data"));
            remove_registration(true);
            assert!(read_string(SETTINGS, w!("collection")).is_none());
            assert_eq!(read_string(OTHER_SETTINGS, w!("collection")).as_deref(), Some("snowhush data"));
            assert_eq!(read_string(OTHER_UNINSTALL, w!("DisplayName")).as_deref(), Some("Snowhush"));
            assert_eq!(read_string(RUN, OTHER_STARTUP).as_deref(), Some(other_command.as_str()));

            let cleanup = folder.join("cleanup-old");
            let reinstalled = folder.join("cleanup-reinstalled");
            std::fs::create_dir_all(&cleanup).unwrap();
            std::fs::create_dir_all(&reinstalled).unwrap();
            std::fs::write(cleanup.join("VelvetFrost.exe"), b"old").unwrap();
            std::fs::write(reinstalled.join("VelvetFrost.exe"), b"old").unwrap();
            std::fs::write(cleanup.join("LICENSE.txt"), crate::license::TEXT).unwrap();
            std::fs::write(reinstalled.join("LICENSE.txt"), crate::license::TEXT).unwrap();
            for name in ["THIRD-PARTY-NOTICES.txt", "RUST-STANDARD-LIBRARY-NOTICES.html"] {
                std::fs::write(cleanup.join(name), b"notice").unwrap();
                std::fs::write(reinstalled.join(name), b"notice").unwrap();
            }
            let mut old_helper = schedule_cleanup(&cleanup).unwrap();
            let mut new_helper = schedule_cleanup(&reinstalled).unwrap();
            std::fs::write(reinstalled.join("VelvetFrost.exe"), b"replacement build").unwrap();
            assert!(old_helper.wait().unwrap().success());
            assert!(new_helper.wait().unwrap().success());
            assert!(!cleanup.exists());
            assert_eq!(std::fs::read(reinstalled.join("VelvetFrost.exe")).unwrap(), b"replacement build");
            assert_eq!(std::fs::read_to_string(reinstalled.join("LICENSE.txt")).unwrap(), crate::license::TEXT);
            assert!(reinstalled.join("THIRD-PARTY-NOTICES.txt").exists());
            assert!(reinstalled.join("RUST-STANDARD-LIBRARY-NOTICES.html").exists());
            return;
        }

        let id = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let root = format!("Software\\VelvetFrostInstallTest-{id}");
        let root_wide: Vec<u16> = root.encode_utf16().chain([0]).collect();
        let folder = std::env::temp_dir().join(format!("VelvetFrostInstallTest-{id}"));
        std::fs::create_dir(&folder).unwrap();
        let mut key = HKEY::default();
        unsafe {
            assert_eq!(
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(root_wide.as_ptr()),
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
            .args(["install::tests::install_update_and_uninstall_preserve_unrelated_snowhush_data", "--exact", "--nocapture"])
            .env(CHILD, &root)
            .env(FOLDER, &folder)
            .output()
            .unwrap();
        assert_eq!(unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(root_wide.as_ptr())) }, ERROR_SUCCESS);
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let checked = folder.canonicalize().unwrap();
        assert!(checked.starts_with(&temp) && checked.file_name().unwrap().to_string_lossy().starts_with("VelvetFrostInstallTest-"));
        std::fs::remove_dir_all(&checked).unwrap();
        assert!(
            output.status.success(),
            "isolated install test failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
