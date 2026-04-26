//! Wooting Analog SDK wrapper — loaded at runtime via `libloading`.
//!
//! We talk to the SDK exclusively through its C ABI (defined in
//! `includes/wooting-analog-sdk.h`). Only the subset of functions needed for
//! `list`, `load`, and `serve` are bound.

use anyhow::{Context, Result, anyhow};
use libloading::{Library, Symbol};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub type DeviceId = u64;

/// Keycode modes; we always use HID.
#[allow(dead_code)]
#[repr(i32)]
pub enum KeycodeType {
    Hid = 0,
    ScanCode1 = 1,
    VirtualKey = 2,
    VirtualKeyTranslate = 3,
}

/// The FFI-facing `DeviceInfo` struct from the SDK header (see
/// `WootingAnalog_DeviceInfo_FFI`). Field order and layout must match exactly.
#[repr(C)]
pub struct RawDeviceInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer_name: *mut c_char,
    pub device_name: *mut c_char,
    pub device_id: u64,
    pub device_type: i32,
}

/// Owned, safe-to-pass-around device info.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: String,
    pub name: String,
    pub id: DeviceId,
    pub kind: i32,
}

pub struct AnalogSdk {
    _lib: Library,
    initialised: bool,
    f_initialise: unsafe extern "C" fn() -> i32,
    f_uninitialise: unsafe extern "C" fn() -> i32,
    f_set_keycode_mode: unsafe extern "C" fn(i32) -> i32,
    f_get_devices: unsafe extern "C" fn(*mut *const RawDeviceInfo, i32) -> i32,
    f_read_full_buffer_device:
        unsafe extern "C" fn(*mut u16, *mut f32, i32, DeviceId) -> i32,
}

impl AnalogSdk {
    /// Load the SDK library and initialise it.
    pub fn open() -> Result<Self> {
        let dll = find_analog_dll()?;
        let lib = unsafe { Library::new(&dll) }
            .with_context(|| format!("loading {}", dll.display()))?;

        unsafe {
            let f_initialise: Symbol<unsafe extern "C" fn() -> i32> = lib
                .get(b"wooting_analog_initialise\0")
                .context("wooting_analog_initialise not found")?;
            let f_uninitialise: Symbol<unsafe extern "C" fn() -> i32> = lib
                .get(b"wooting_analog_uninitialise\0")
                .context("wooting_analog_uninitialise not found")?;
            let f_set_keycode_mode: Symbol<unsafe extern "C" fn(i32) -> i32> = lib
                .get(b"wooting_analog_set_keycode_mode\0")
                .context("wooting_analog_set_keycode_mode not found")?;
            let f_get_devices: Symbol<
                unsafe extern "C" fn(*mut *const RawDeviceInfo, i32) -> i32,
            > = lib
                .get(b"wooting_analog_get_connected_devices_info\0")
                .context("wooting_analog_get_connected_devices_info not found")?;
            let f_read_full_buffer_device: Symbol<
                unsafe extern "C" fn(*mut u16, *mut f32, i32, DeviceId) -> i32,
            > = lib
                .get(b"wooting_analog_read_full_buffer_device\0")
                .context("wooting_analog_read_full_buffer_device not found")?;

            let sdk = AnalogSdk {
                f_initialise: *f_initialise,
                f_uninitialise: *f_uninitialise,
                f_set_keycode_mode: *f_set_keycode_mode,
                f_get_devices: *f_get_devices,
                f_read_full_buffer_device: *f_read_full_buffer_device,
                initialised: false,
                _lib: lib,
            };
            Ok(sdk)
        }
    }

    /// Initialise + switch to HID keycode mode.
    pub fn initialise(&mut self) -> Result<i32> {
        let rc = unsafe { (self.f_initialise)() };
        if rc < 0 {
            anyhow::bail!("wooting_analog_initialise failed (rc={rc})");
        }
        self.initialised = true;
        // HID keycode mode = 0.
        let _ = unsafe { (self.f_set_keycode_mode)(KeycodeType::Hid as i32) };
        Ok(rc)
    }

    /// Enumerate currently connected Wooting devices.
    pub fn connected_devices(&self, max: i32) -> Result<Vec<DeviceInfo>> {
        // The SDK writes up to `max` pointers into the buffer we supply.
        let mut buf: Vec<*const RawDeviceInfo> = vec![std::ptr::null(); max as usize];
        let rc = unsafe { (self.f_get_devices)(buf.as_mut_ptr(), max) };
        if rc < 0 {
            anyhow::bail!("wooting_analog_get_connected_devices_info failed (rc={rc})");
        }
        let count = rc.max(0) as usize;
        let mut out = Vec::with_capacity(count);
        for p in buf.iter().take(count) {
            if p.is_null() {
                continue;
            }
            let info = unsafe { &**p };
            let manufacturer = unsafe { cstr_to_string(info.manufacturer_name) };
            let name = unsafe { cstr_to_string(info.device_name) };
            out.push(DeviceInfo {
                vendor_id: info.vendor_id,
                product_id: info.product_id,
                manufacturer,
                name,
                id: info.device_id,
                kind: info.device_type,
            });
        }
        Ok(out)
    }

    /// Read the full analog buffer for one device.
    /// Returns `Vec<(hid_code, analog_depth 0.0..1.0)>`.
    pub fn read_full_buffer(&self, device: DeviceId, max: usize) -> Result<Vec<(u16, f32)>> {
        let mut codes = vec![0u16; max];
        let mut analogs = vec![0.0f32; max];
        let rc = unsafe {
            (self.f_read_full_buffer_device)(
                codes.as_mut_ptr(),
                analogs.as_mut_ptr(),
                max as i32,
                device,
            )
        };
        if rc < 0 {
            anyhow::bail!("wooting_analog_read_full_buffer_device failed (rc={rc})");
        }
        let n = rc.max(0) as usize;
        Ok(codes.into_iter().zip(analogs).take(n).collect())
    }
}

impl Drop for AnalogSdk {
    fn drop(&mut self) {
        if self.initialised {
            unsafe { (self.f_uninitialise)() };
        }
    }
}

unsafe fn cstr_to_string(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

fn find_analog_dll() -> Result<PathBuf> {
    // Standard install locations, most specific first.
    let candidates: &[&str] = &[
        // Windows MSI install.
        r"C:\Program Files\wooting-analog-sdk\wooting_analog_sdk.dll",
        // Linux/Raspberry Pi defaults.
        "/usr/local/lib/libwooting_analog_sdk.so",
        "/usr/lib/libwooting_analog_sdk.so",
        "/usr/lib/x86_64-linux-gnu/libwooting_analog_sdk.so",
        "/usr/lib/aarch64-linux-gnu/libwooting_analog_sdk.so",
    ];
    for c in candidates {
        let p = Path::new(c);
        if p.exists() {
            return Ok(p.to_path_buf());
        }
    }
    // Env override: WOOTING_ANALOG_SDK_DLL=/path/to/lib
    if let Ok(p) = std::env::var("WOOTING_ANALOG_SDK_DLL") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }
    Err(anyhow!(
        "Wooting Analog SDK not found. Run `scripts/install-wooting-sdks.ps1` (Windows) \
         or `scripts/install-wooting-sdks.sh` (Linux), or set WOOTING_ANALOG_SDK_DLL."
    ))
}

/// Thread-safe global so we can enumerate + poll from the main loop + list command
/// without re-initialising. Lazy-init on first use.
static SDK: Mutex<Option<AnalogSdk>> = Mutex::new(None);

pub fn with_sdk<R>(f: impl FnOnce(&mut AnalogSdk) -> Result<R>) -> Result<R> {
    let mut guard = SDK.lock().unwrap();
    if guard.is_none() {
        let mut sdk = AnalogSdk::open()?;
        sdk.initialise()?;
        *guard = Some(sdk);
    }
    f(guard.as_mut().unwrap())
}
