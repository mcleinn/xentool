//! Wooting RGB SDK wrapper — loaded at runtime via `libloading`.

use anyhow::{Context, Result, anyhow};
use libloading::{Library, Symbol};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct RgbSdk {
    _lib: Library,
    f_kbd_connected: unsafe extern "C" fn() -> bool,
    f_device_count: unsafe extern "C" fn() -> u8,
    f_select_device: unsafe extern "C" fn(u8) -> bool,
    f_array_auto_update: unsafe extern "C" fn(bool),
    f_array_set_single: unsafe extern "C" fn(u8, u8, u8, u8, u8) -> bool,
    f_array_update_keyboard: unsafe extern "C" fn() -> bool,
    f_direct_set_key: unsafe extern "C" fn(u8, u8, u8, u8, u8) -> bool,
}

impl RgbSdk {
    pub fn open() -> Result<Self> {
        let dll = find_rgb_dll()?;
        let lib = unsafe { Library::new(&dll) }
            .with_context(|| format!("loading {}", dll.display()))?;

        unsafe {
            let f_kbd_connected: Symbol<unsafe extern "C" fn() -> bool> = lib
                .get(b"wooting_rgb_kbd_connected\0")
                .context("wooting_rgb_kbd_connected not found")?;
            let f_device_count: Symbol<unsafe extern "C" fn() -> u8> = lib
                .get(b"wooting_usb_device_count\0")
                .context("wooting_usb_device_count not found")?;
            let f_select_device: Symbol<unsafe extern "C" fn(u8) -> bool> = lib
                .get(b"wooting_usb_select_device\0")
                .context("wooting_usb_select_device not found")?;
            let f_array_auto_update: Symbol<unsafe extern "C" fn(bool)> = lib
                .get(b"wooting_rgb_array_auto_update\0")
                .context("wooting_rgb_array_auto_update not found")?;
            let f_array_set_single: Symbol<
                unsafe extern "C" fn(u8, u8, u8, u8, u8) -> bool,
            > = lib
                .get(b"wooting_rgb_array_set_single\0")
                .context("wooting_rgb_array_set_single not found")?;
            let f_array_update_keyboard: Symbol<unsafe extern "C" fn() -> bool> = lib
                .get(b"wooting_rgb_array_update_keyboard\0")
                .context("wooting_rgb_array_update_keyboard not found")?;
            let f_direct_set_key: Symbol<unsafe extern "C" fn(u8, u8, u8, u8, u8) -> bool> = lib
                .get(b"wooting_rgb_direct_set_key\0")
                .context("wooting_rgb_direct_set_key not found")?;

            let sdk = RgbSdk {
                f_kbd_connected: *f_kbd_connected,
                f_device_count: *f_device_count,
                f_select_device: *f_select_device,
                f_array_auto_update: *f_array_auto_update,
                f_array_set_single: *f_array_set_single,
                f_array_update_keyboard: *f_array_update_keyboard,
                f_direct_set_key: *f_direct_set_key,
                _lib: lib,
            };
            // Prime the connection check and disable auto-update so we batch.
            let _ = (sdk.f_kbd_connected)();
            (sdk.f_array_auto_update)(false);
            Ok(sdk)
        }
    }

    pub fn device_count(&self) -> u8 {
        unsafe { (self.f_device_count)() }
    }

    /// Select a device by index (SDK state is global — re-select before every batch).
    pub fn select(&self, device_index: u8) -> Result<()> {
        let ok = unsafe { (self.f_select_device)(device_index) };
        if !ok {
            anyhow::bail!("wooting_usb_select_device({device_index}) failed");
        }
        Ok(())
    }

    /// Set one LED immediately (non-batched).
    pub fn direct_set_key(&self, device_index: u8, row: u8, col: u8, rgb: (u8, u8, u8)) -> Result<()> {
        self.select(device_index)?;
        let ok = unsafe { (self.f_direct_set_key)(row, col, rgb.0, rgb.1, rgb.2) };
        if !ok {
            anyhow::bail!("wooting_rgb_direct_set_key failed");
        }
        Ok(())
    }

    /// Stage a single-cell color update (buffered until `array_update_keyboard`).
    pub fn array_set_single(&self, device_index: u8, row: u8, col: u8, rgb: (u8, u8, u8)) -> Result<()> {
        self.select(device_index)?;
        let ok = unsafe { (self.f_array_set_single)(row, col, rgb.0, rgb.1, rgb.2) };
        if !ok {
            anyhow::bail!("wooting_rgb_array_set_single failed");
        }
        Ok(())
    }

    /// Flush buffered updates to the keyboard.
    pub fn array_update_keyboard(&self, device_index: u8) -> Result<()> {
        self.select(device_index)?;
        let ok = unsafe { (self.f_array_update_keyboard)() };
        if !ok {
            anyhow::bail!("wooting_rgb_array_update_keyboard failed");
        }
        Ok(())
    }
}

fn find_rgb_dll() -> Result<PathBuf> {
    let candidates: &[&str] = &[
        r"C:\Program Files\wooting-rgb-sdk\wooting-rgb-sdk.dll",
        r"C:\Program Files\Common Files\Wooting\wooting-rgb-sdk.dll",
        "/usr/local/lib/libwooting-rgb-sdk.so",
        "/usr/lib/libwooting-rgb-sdk.so",
        "/usr/lib/x86_64-linux-gnu/libwooting-rgb-sdk.so",
        "/usr/lib/aarch64-linux-gnu/libwooting-rgb-sdk.so",
    ];
    for c in candidates {
        let p = Path::new(c);
        if p.exists() {
            return Ok(p.to_path_buf());
        }
    }
    // User-local fallback + env override.
    if let Ok(home) = std::env::var("LOCALAPPDATA") {
        let p = Path::new(&home).join("wooting-rgb-sdk").join("wooting-rgb-sdk.dll");
        if p.exists() {
            return Ok(p);
        }
    }
    if let Ok(p) = std::env::var("WOOTING_RGB_SDK_DLL") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Ok(path);
        }
    }
    Err(anyhow!(
        "Wooting RGB SDK not found. Run `scripts/install-wooting-sdks.ps1` (Windows) \
         or `scripts/install-wooting-sdks.sh` (Linux), or set WOOTING_RGB_SDK_DLL."
    ))
}

static SDK: Mutex<Option<RgbSdk>> = Mutex::new(None);

pub fn with_sdk<R>(f: impl FnOnce(&RgbSdk) -> Result<R>) -> Result<R> {
    let mut guard = SDK.lock().unwrap();
    if guard.is_none() {
        *guard = Some(RgbSdk::open()?);
    }
    f(guard.as_ref().unwrap())
}
