//! System-audio loopback capture: resolving the right `DeviceSpec` for the
//! current OS, and reporting when the platform can't do it.
//!
//! Meeting mode captures what comes OUT of the speakers (everyone else on the
//! call) alongside the microphone (you):
//!   - Windows: cpal's default output device opened as an input = WASAPI loopback.
//!   - macOS 14.6+: a GLOBAL CoreAudio process tap (`meeting/system_tap.rs`),
//!     device-independent — it captures all process audio pre-routing rather
//!     than being bound to one output device's UID. cpal's device-bound tap
//!     (opening a specific output as an input) silently delivered silence for
//!     some outputs, notably Bluetooth, which is why this isn't cpal-based.
//!   - Linux: the default sink's `.monitor` source, enumerated as an input by
//!     the native PipeWire/PulseAudio hosts (the `linux-audio-hosts` feature).
//!
//! When capture isn't possible (macOS < 14.6, no monitor source, headless CI),
//! `resolve()` returns `Unsupported(reason)` and Meeting mode records mic-only
//! with that reason surfaced to the UI.

use crate::recorder::DeviceSpec;

/// Outcome of probing for a loopback capture device.
pub enum Loopback {
    /// The spec to hand a second `AudioRecorder`.
    Available(DeviceSpec),
    /// Loopback can't run here; the string is a user-facing reason.
    Unsupported(String),
}

/// Resolve system-audio capture for the current platform. `preferred_output` is
/// a specific output device name the user chose in Meeting settings (None for
/// the default output). Honored on Windows/macOS, where loopback opens an OUTPUT
/// device as input; ignored on Linux, where loopback is a monitor input source.
pub fn resolve(preferred_output: Option<&str>) -> Loopback {
    #[cfg(target_os = "windows")]
    {
        // WASAPI loopback: any output device works, opened as input.
        Loopback::Available(output_spec(preferred_output))
    }
    #[cfg(target_os = "macos")]
    {
        resolve_macos(preferred_output)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = preferred_output; // not yet mapped to a specific monitor source
        resolve_linux()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = preferred_output;
        Loopback::Unsupported("System audio capture isn't supported on this platform".into())
    }
}

/// Loopback spec for Windows: the named output when the user picked one,
/// else the default output.
#[cfg(target_os = "windows")]
fn output_spec(preferred: Option<&str>) -> DeviceSpec {
    match preferred {
        Some(name) if !name.is_empty() => DeviceSpec::LoopbackByName(name.to_string()),
        _ => DeviceSpec::LoopbackDefaultOutput,
    }
}

/// A global CoreAudio process tap needs macOS 14.6+. Gate on the runtime OS
/// version (a build could run on an older host than it was compiled
/// against), falling back to mic-only with a clear reason below the floor.
/// `preferred_output` is unused here: a global tap captures all process
/// audio pre-routing, independent of whichever output device is selected.
#[cfg(target_os = "macos")]
fn resolve_macos(preferred_output: Option<&str>) -> Loopback {
    let _ = preferred_output;
    match macos_product_version() {
        Some((major, minor)) if (major, minor) >= (14, 6) => {
            Loopback::Available(DeviceSpec::SystemTapGlobal)
        }
        Some((major, minor)) => Loopback::Unsupported(format!(
            "System audio capture needs macOS 14.6 or newer (this is {major}.{minor}). Recording microphone only."
        )),
        None => Loopback::Unsupported(
            "Couldn't determine the macOS version for system audio capture. Recording microphone only.".into(),
        ),
    }
}

/// Read `kern.osproductversion` (e.g. "14.6.1") and parse major.minor.
#[cfg(target_os = "macos")]
fn macos_product_version() -> Option<(u32, u32)> {
    use std::ffi::CString;
    let name = CString::new("kern.osproductversion").ok()?;
    let mut size: libc::size_t = 0;
    // First call sizes the buffer.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    // Trim the trailing NUL sysctl includes.
    if let Some(&0) = buf.last() {
        buf.pop();
    }
    let version = String::from_utf8(buf).ok()?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Some((major, minor))
}

/// Linux monitor sources appear as input devices named like
/// "Monitor of <sink>" (or ending in ".monitor") under the PipeWire/PulseAudio
/// hosts. Pick one by name so the recorder can open it directly.
#[cfg(target_os = "linux")]
fn resolve_linux() -> Loopback {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let inputs = match host.input_devices() {
        Ok(i) => i,
        Err(e) => return Loopback::Unsupported(format!("Couldn't enumerate audio inputs: {e}")),
    };
    for dev in inputs {
        if let Ok(desc) = dev.description() {
            let name = desc.name();
            let lower = name.to_lowercase();
            if lower.ends_with(".monitor") || lower.contains("monitor of") {
                return Loopback::Available(DeviceSpec::InputByName(name.to_string()));
            }
        }
    }
    Loopback::Unsupported(
        "No system-audio monitor source found. Enable PipeWire/PulseAudio, or record microphone only.".into(),
    )
}
