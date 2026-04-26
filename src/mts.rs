use std::ffi::CString;
use std::path::Path;

use anyhow::{Context, Result, bail};
use libloading::{Library, Symbol};

const C0_HZ: f64 = 16.351_597_831_287_414; // A4=440 Hz reference

pub fn edo_freq_hz(divisions: i32, pitch_index: i32) -> f64 {
    C0_HZ * 2f64.powf(pitch_index as f64 / divisions as f64)
}

/// Runtime-loaded MTS-ESP master.
pub struct MtsMaster {
    _lib: Library,
    deregister: unsafe extern "C" fn(),
}

impl MtsMaster {
    /// Load LIBMTS.dll and register as MTS-ESP master.
    pub fn register() -> Result<Self> {
        let dll_path = find_libmts()?;
        eprintln!("mts: loading {}", dll_path.display());
        let lib = unsafe { Library::new(&dll_path) }
            .with_context(|| format!("failed to load {}", dll_path.display()))?;
        eprintln!("mts: library loaded");

        unsafe {
            let has_ipc: Symbol<unsafe extern "C" fn() -> bool> = lib
                .get(b"MTS_HasIPC\0")
                .context("MTS_HasIPC not found in LIBMTS.dll")?;
            let ipc = has_ipc();
            eprintln!("mts: HasIPC = {ipc}");
            if ipc {
                let reinit: Symbol<unsafe extern "C" fn()> = lib
                    .get(b"MTS_Reinitialize\0")
                    .context("MTS_Reinitialize not found")?;
                reinit();
                eprintln!("mts: Reinitialize called");
            }

            // Check if we can register
            if let Ok(can_register) = lib.get::<unsafe extern "C" fn() -> bool>(b"MTS_CanRegisterMaster\0") {
                let can = can_register();
                eprintln!("mts: CanRegisterMaster = {can}");
                if !can {
                    bail!("Another MTS-ESP master is already registered. Close the MTS-ESP Master plugin in your DAW first.");
                }
            } else {
                eprintln!("mts: MTS_CanRegisterMaster not available (older LIBMTS)");
            }

            let register_fn: Symbol<unsafe extern "C" fn()> = lib
                .get(b"MTS_RegisterMaster\0")
                .context("MTS_RegisterMaster not found in LIBMTS.dll")?;
            register_fn();
            eprintln!("mts: RegisterMaster called");
        }

        let deregister: unsafe extern "C" fn() = unsafe {
            *lib.get::<unsafe extern "C" fn()>(b"MTS_DeregisterMaster\0")
                .context("MTS_DeregisterMaster not found")?
        };

        Ok(Self {
            _lib: lib,
            deregister,
        })
    }

    pub fn set_scale_name(&self, name: &str) -> Result<()> {
        let s = CString::new(name).context("scale name contains NUL")?;
        unsafe {
            let func: Symbol<unsafe extern "C" fn(*const i8)> = self
                ._lib
                .get(b"MTS_SetScaleName\0")
                .context("MTS_SetScaleName not found")?;
            func(s.as_ptr());
        }
        Ok(())
    }

    pub fn get_num_clients(&self) -> i32 {
        unsafe {
            if let Ok(func) = self._lib.get::<unsafe extern "C" fn() -> i32>(b"MTS_GetNumClients\0")
            {
                func()
            } else {
                -1
            }
        }
    }

    pub fn set_note_tunings(&self, freqs: &[f64; 128]) -> Result<()> {
        unsafe {
            let func: Symbol<unsafe extern "C" fn(*const f64)> = self
                ._lib
                .get(b"MTS_SetNoteTunings\0")
                .context("MTS_SetNoteTunings not found")?;
            func(freqs.as_ptr());
        }
        Ok(())
    }
}

impl Drop for MtsMaster {
    fn drop(&mut self) {
        unsafe { (self.deregister)() };
    }
}

fn find_libmts() -> Result<std::path::PathBuf> {
    // Standard Windows path
    let standard = Path::new(r"C:\Program Files\Common Files\MTS-ESP\LIBMTS.dll");
    if standard.exists() {
        return Ok(standard.to_path_buf());
    }

    // 32-bit fallback
    let x86 = Path::new(r"C:\Program Files (x86)\Common Files\MTS-ESP\LIBMTS.dll");
    if x86.exists() {
        return Ok(x86.to_path_buf());
    }

    // Linux path
    let linux = Path::new("/usr/local/lib/libMTS.so");
    if linux.exists() {
        return Ok(linux.to_path_buf());
    }

    bail!(
        "LIBMTS not found. Install MTS-ESP from https://oddsound.com/mtsespmini.php\n\
         Expected at: {}",
        standard.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edo_12_c0_is_base() {
        let freq = edo_freq_hz(12, 0); // pitch_index 0 = C0
        assert!((freq - C0_HZ).abs() < 0.001, "pitch 0 should be C0 = {C0_HZ} Hz, got {freq}");
    }

    #[test]
    fn edo_12_a4_is_440() {
        // A4 = 440 Hz. In the C0-based system, A4 is 57 half-steps above C0
        // (4 octaves + 9 semitones = 48 + 9 = 57)
        let freq = edo_freq_hz(12, 57);
        assert!((freq - 440.0).abs() < 0.01, "A4 should be ~440 Hz, got {freq}");
    }

    #[test]
    fn edo_31_octave_doubles() {
        let f0 = edo_freq_hz(31, 0);
        let f31 = edo_freq_hz(31, 31);
        assert!((f31 / f0 - 2.0).abs() < 0.001, "31 steps should double frequency");
    }
}
