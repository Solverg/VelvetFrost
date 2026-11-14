//! Background download and verification of releases from the VelvetFrost fork.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle,
    WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

use snowhush::update_security::{self, Manifest};
use snowhush_core::text::{self, tr};

const REPOSITORY: &str = match option_env!("VELVETFROST_UPDATE_REPOSITORY") {
    Some(repository) => repository,
    None => "Solverg/VelvetFrost",
};
const PUBLIC_KEY: Option<&str> = option_env!("VELVETFROST_UPDATE_PUBLIC_KEY");
const INSTALLER: &str = "VelvetFrost-Setup.exe";
const MANIFEST: &str = "VelvetFrost-update.txt";
const SIGNATURE: &str = "VelvetFrost-update.sig";

/// Holds the verified file open without write/delete sharing until installation.
pub struct Release {
    pub version: String,
    path: PathBuf,
    file: Option<File>,
    manifest: Manifest,
}

impl Release {
    pub fn install(&mut self) -> Result<Child, String> {
        let actual = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
        let installed = snowhush::install::installed_exe().and_then(|p| p.canonicalize().ok());
        if !matches!((actual, installed), (Some(actual), Some(installed)) if actual.to_string_lossy().eq_ignore_ascii_case(&installed.to_string_lossy()))
        {
            return Err(tr("update.installed_only").to_string());
        }
        let file = self.file.as_mut().ok_or_else(|| tr("update.file_error").to_string())?;
        update_security::verify_installer(file, &self.manifest).map_err(|_| tr("update.verify_error").to_string())?;
        // Keep the no-write/no-delete-sharing handle open through CreateProcess.
        Command::new(&self.path).arg("--apply-update").spawn().map_err(|error| error.to_string())
    }
}

impl Drop for Release {
    fn drop(&mut self) {
        drop(self.file.take());
        // If the installer is already running Windows can refuse deletion. Its
        // file remains in the user's temporary directory for Windows cleanup.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Never accept a key supplied by a release or downloaded from the network.
fn public_key() -> Result<[u8; 32], String> {
    let text = PUBLIC_KEY.ok_or_else(|| tr("update.key_error").to_string())?;
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(tr("update.key_error").to_string());
    }
    let mut key = [0; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|_| tr("update.key_error").to_string())?;
    }
    Ok(key)
}

fn valid_repository(repository: &str) -> bool {
    let parts: Vec<_> = repository.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty() && *part != "." && *part != ".." && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
}

/// Check and fully verify a release before it can be announced.
pub fn check() -> Result<Option<Release>, String> {
    check_with(REPOSITORY, env!("CARGO_PKG_VERSION"), public_key, fetch, verify_file_version)
}

fn check_with(
    repository: &str,
    current: &str,
    get_key: impl FnOnce() -> Result<[u8; 32], String>,
    mut get: impl FnMut(&str, &str, &mut dyn Write, u64) -> Result<(), String>,
    verify_version: impl FnOnce(&Path, &str) -> Result<(), String>,
) -> Result<Option<Release>, String> {
    if !valid_repository(repository) {
        return Err(tr("update.unconfigured").to_string());
    }
    let mut response = Vec::new();
    get("api.github.com", &format!("/repos/{repository}/releases/latest"), &mut response, 1024 * 1024)?;
    let release: Value = serde_json::from_slice(&response).map_err(|_| tr("update.response_error").to_string())?;
    if release["draft"].as_bool() != Some(false) || release["prerelease"].as_bool() != Some(false) {
        return Err(tr("update.response_error").to_string());
    }
    let tag = release["tag_name"].as_str().ok_or_else(|| tr("update.no_tag").to_string())?;
    if !update_security::newer_tag(tag, current).map_err(|_| tr("update.response_error").to_string())? {
        return Ok(None);
    }
    let assets = release["assets"].as_array().ok_or_else(|| tr("update.response_error").to_string())?;
    for name in [INSTALLER, MANIFEST, SIGNATURE] {
        if assets.iter().filter(|asset| asset["name"].as_str() == Some(name) && asset["state"].as_str() == Some("uploaded")).count() != 1 {
            return Err(tr("update.assets_error").to_string());
        }
    }
    let key = get_key()?;
    let path = |name: &str| format!("/{repository}/releases/download/{tag}/{name}");
    let mut manifest_bytes = Vec::new();
    get("github.com", &path(MANIFEST), &mut manifest_bytes, 1024)?;
    let mut signature = Vec::new();
    get("github.com", &path(SIGNATURE), &mut signature, 64)?;
    let manifest =
        update_security::verify_manifest(&manifest_bytes, &signature, &key, tag, current).map_err(|_| tr("update.verify_error").to_string())?;
    let (path, mut download) = temporary_file()?;
    let result = get("github.com", &format!("/{repository}/releases/download/{tag}/{INSTALLER}"), &mut download, manifest.size)
        .and_then(|()| download.flush().map_err(|error| error.to_string()));
    drop(download);
    if let Err(error) = result {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    // The writer must be closed before the loader can open the executable.
    // Verify only after opening the read-only handle that remains locked.
    let mut file = match OpenOptions::new().read(true).share_mode(1).open(&path) {
        Ok(file) => file,
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            return Err(error.to_string());
        }
    };
    if update_security::verify_installer(&mut file, &manifest).is_err() {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(tr("update.verify_error").to_string());
    }
    if let Err(error) = verify_version(&path, &manifest.version) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(Some(Release { version: manifest.version.clone(), path, file: Some(file), manifest }))
}

fn verify_file_version(path: &Path, expected: &str) -> Result<(), String> {
    let invalid = || tr("update.verify_error").to_string();
    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(path.as_ptr()), None);
        if size == 0 || size > 1024 * 1024 {
            return Err(invalid());
        }
        let mut bytes = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(path.as_ptr()), None, size, bytes.as_mut_ptr().cast()).map_err(|_| invalid())?;
        let mut value = std::ptr::null_mut();
        let mut length = 0u32;
        if !VerQueryValueW(bytes.as_ptr().cast(), w!("\\"), &mut value, &mut length).as_bool()
            || value.is_null()
            || length < std::mem::size_of::<VS_FIXEDFILEINFO>() as u32
        {
            return Err(invalid());
        }
        let info = std::ptr::read_unaligned(value.cast::<VS_FIXEDFILEINFO>());
        let actual = format!("{}.{}.{}", info.dwFileVersionMS >> 16, info.dwFileVersionMS & 0xffff, info.dwFileVersionLS >> 16);
        if info.dwSignature != 0xfeef04bd
            || info.dwFileVersionLS & 0xffff != 0
            || actual != expected
            || info.dwProductVersionMS != info.dwFileVersionMS
            || info.dwProductVersionLS != info.dwFileVersionLS
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn temporary_file() -> Result<(PathBuf, File), String> {
    let mut random = [0u8; 16];
    SystemRandom::new().fill(&mut random).map_err(|_| tr("update.file_error").to_string())?;
    let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let path = std::env::temp_dir().join(format!("VelvetFrost-update-{suffix}.exe"));
    // FILE_SHARE_READ permits the Windows loader to open it but prevents writes
    // and rename/delete while the verified handle is held.
    let file = OpenOptions::new().read(true).write(true).create_new(true).share_mode(1).open(&path).map_err(|error| error.to_string())?;
    Ok((path, file))
}

pub fn open(url: &str) {
    let url: Vec<u16> = url.encode_utf16().chain([0]).collect();
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn fetch(host: &str, path: &str, output: &mut dyn Write, limit: u64) -> Result<(), String> {
    let failed = |key: &str| text::fill(key, &[("error", &windows::core::Error::from_thread().message())]);
    let accept = if host == "api.github.com" { "Accept: application/vnd.github+json\r\n" } else { "Accept: application/octet-stream\r\n" };
    let host: Vec<u16> = host.encode_utf16().chain([0]).collect();
    let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
    unsafe {
        let session = Handle(WinHttpOpen(w!("VelvetFrost"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return Err(failed("update.connection"));
        }
        let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 10_000, 10_000);
        let connection = Handle(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), 443, 0));
        if connection.0.is_null() {
            return Err(failed("update.github"));
        }
        let request = Handle(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(failed("update.request"));
        }
        let headers: Vec<u16> = accept.encode_utf16().collect();
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0).map_err(|error| error.message())?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|error| error.message())?;
        let mut status = 0u32;
        let mut size = 4u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|error| error.message())?;
        if status != 200 {
            return Err(text::fill("update.status", &[("status", &status.to_string())]));
        }
        let mut total = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, buffer.as_mut_ptr().cast(), buffer.len() as u32, &mut read).map_err(|error| error.message())?;
            if read == 0 {
                break;
            }
            total = total.checked_add(read as u64).ok_or_else(|| tr("update.file_error").to_string())?;
            if total > limit {
                return Err(tr("update.file_error").to_string());
            }
            output.write_all(&buffer[..read as usize]).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{digest, signature::KeyPair};
    use std::process::Stdio;

    #[test]
    fn locked_executable_can_start_but_cannot_be_replaced() {
        let (path, writer) = temporary_file().unwrap();
        drop(writer);
        std::fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
        let reader = OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::rename(&path, path.with_extension("moved")).is_err());
        assert!(verify_file_version(&path, env!("CARGO_PKG_VERSION")).is_ok());
        let mut child = Command::new(&path).arg("--list").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        assert!(child.wait().unwrap().success());
        drop(reader);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_unsafe_repository_names() {
        assert!(valid_repository("Solverg/VelvetFrost"));
        for bad in ["", "a/b/c", "../x", "a/..", "a/x?", "a/b c"] {
            assert!(!valid_repository(bad));
        }
    }

    #[test]
    fn embedded_public_key_matches_the_project_pin() {
        if option_env!("VELVETFROST_TEST_UPDATE_PUBLIC_KEY").is_some() {
            return;
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("update-public-key.hex");
        let Ok(pin) = std::fs::read_to_string(path) else { return };
        let expected = pin.trim().to_ascii_lowercase();
        let actual: String = public_key().unwrap().iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn background_check_only_returns_a_verified_download() {
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[19; 32]).unwrap();
        let public: [u8; 32] = key.public_key().as_ref().try_into().unwrap();
        let installer = b"test installer".to_vec();
        let hash: String = digest::digest(&digest::SHA256, &installer).as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
        let manifest = format!("VelvetFrost update manifest v1\n0.20.0\n{INSTALLER}\n{}\n{hash}\n", installer.len()).into_bytes();
        let sig = key.sign(&manifest).as_ref().to_vec();
        let metadata = |tag: &str| {
            serde_json::json!({
                "draft": false, "prerelease": false, "tag_name": tag,
                "assets": [
                    {"name": INSTALLER, "state": "uploaded"},
                    {"name": MANIFEST, "state": "uploaded"},
                    {"name": SIGNATURE, "state": "uploaded"}
                ]
            })
            .to_string()
            .into_bytes()
        };
        let run = |tag: &str, payload: &[u8], signature: &[u8], offline: bool| {
            let metadata = metadata(tag);
            check_with(
                "Solverg/VelvetFrost",
                "0.19.0",
                || Ok(public),
                |host, path, output, limit| {
                    if offline {
                        return Err("network error".into());
                    }
                    let bytes: &[u8] = if host == "api.github.com" {
                        &metadata
                    } else if path.ends_with(MANIFEST) {
                        &manifest
                    } else if path.ends_with(SIGNATURE) {
                        signature
                    } else if path.ends_with(INSTALLER) {
                        payload
                    } else {
                        return Err("unexpected request".into());
                    };
                    if bytes.len() as u64 > limit {
                        return Err("too large".into());
                    }
                    output.write_all(bytes).map_err(|error| error.to_string())
                },
                |_, version| if version == "0.20.0" { Ok(()) } else { Err("wrong file version".into()) },
            )
        };
        let ready = run("v0.20.0", &installer, &sig, false).unwrap().unwrap();
        assert_eq!(ready.version, "0.20.0");
        let path = ready.path.clone();
        assert!(path.exists());
        drop(ready);
        assert!(!path.exists());
        assert!(run("v0.19.0", &installer, &sig, false).unwrap().is_none());
        assert!(run("v0.20.0", &installer, &sig, true).is_err());
        assert!(run("v0.20.0", b"test installes", &sig, false).is_err());
        assert!(run("v0.20.0", &installer, &[0; 64], false).is_err());
        assert!(run("v0.21.0", &installer, &sig, false).is_err());
        assert!(run("v0.20.0", &installer, &sig, false).unwrap().is_some(), "a later retry must succeed");
    }

    #[test]
    fn executable_version_must_match_the_signed_version() {
        let exe = std::env::current_exe().unwrap();
        assert!(verify_file_version(&exe, env!("CARGO_PKG_VERSION")).is_ok());
        assert!(verify_file_version(&exe, "999.0.0").is_err());
        let (path, file) = temporary_file().unwrap();
        drop(file);
        assert!(verify_file_version(&path, env!("CARGO_PKG_VERSION")).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "contacts the public GitHub Releases API"]
    fn public_release_api_is_readable_via_winhttp() {
        let mut response = Vec::new();
        fetch("api.github.com", "/repos/Solverg/VelvetFrost/releases/latest", &mut response, 1024 * 1024).unwrap();
        let json: Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(json["draft"].as_bool(), Some(false));
        assert_eq!(json["prerelease"].as_bool(), Some(false));
        assert!(json["tag_name"].as_str().is_some());
    }

    #[test]
    #[ignore = "downloads the public installer through GitHub redirects"]
    fn public_installer_streams_through_winhttp() {
        struct HashSink {
            hash: digest::Context,
            size: u64,
        }
        impl Write for HashSink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.hash.update(bytes);
                self.size += bytes.len() as u64;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut response = Vec::new();
        fetch("api.github.com", "/repos/Solverg/VelvetFrost/releases/latest", &mut response, 1024 * 1024).unwrap();
        let json: Value = serde_json::from_slice(&response).unwrap();
        let tag = json["tag_name"].as_str().unwrap();
        assert!(update_security::newer_tag(tag, "0.0.0").unwrap());
        let asset = json["assets"].as_array().unwrap().iter().find(|item| item["name"].as_str() == Some(INSTALLER)).unwrap();
        let size = asset["size"].as_u64().unwrap();
        let expected = asset["digest"].as_str().unwrap().strip_prefix("sha256:").unwrap();
        let mut sink = HashSink { hash: digest::Context::new(&digest::SHA256), size: 0 };
        fetch("github.com", &format!("/Solverg/VelvetFrost/releases/download/{tag}/{INSTALLER}"), &mut sink, update_security::MAX_INSTALLER_SIZE)
            .unwrap();
        let actual: String = sink.hash.finish().as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(sink.size, size);
        assert_eq!(actual, expected);
    }
}
