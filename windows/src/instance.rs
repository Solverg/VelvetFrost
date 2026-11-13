//! Snowhush and VelvetFrost deliberately share this per-session exclusion lock.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::{PCWSTR, w};

pub struct Instance(HANDLE);

impl Instance {
    /// Hold the named object for the entire app or installation operation.
    pub fn acquire() -> Option<Self> {
        Self::acquire_named(w!("Local\\Snowhush"))
    }

    fn acquire_named(name: PCWSTR) -> Option<Self> {
        unsafe {
            let handle = CreateMutexW(None, false, name).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_lock_blocks_another_instance_until_released() {
        let name: Vec<u16> = format!("Local\\VelvetFrostLockTest-{}", std::process::id()).encode_utf16().chain([0]).collect();
        let name = PCWSTR(name.as_ptr());
        let first = Instance::acquire_named(name).expect("first instance");
        assert!(Instance::acquire_named(name).is_none());
        drop(first);
        assert!(Instance::acquire_named(name).is_some());
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
