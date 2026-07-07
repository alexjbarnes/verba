//! System-audio loopback capture: resolving the right `DeviceSpec` for the
//! current OS, and reporting when the platform can't do it.
//!
//! Meeting mode captures what comes OUT of the speakers (everyone else on the
//! call) alongside the microphone (you). cpal 0.18 exposes this uniformly:
//!   - Windows: the default output device opened as an input = WASAPI loopback.
//!   - macOS 14.6+: a Core Audio process tap behind the same output-as-input.
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

/// Resolve system-audio capture for the current platform.
pub fn resolve() -> Loopback {
    #[cfg(target_os = "windows")]
    {
        // WASAPI loopback: any default output device works, opened as input.
        Loopback::Available(DeviceSpec::LoopbackDefaultOutput)
    }
    #[cfg(target_os = "macos")]
    {
        resolve_macos()
    }
    #[cfg(target_os = "linux")]
    {
        resolve_linux()
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Loopback::Unsupported("System audio capture isn't supported on this platform".into())
    }
}

/// Core Audio process taps land in cpal 0.18 but need macOS 14.6+. Gate on the
/// runtime OS version (a build could run on an older host than it was compiled
/// against), falling back to mic-only with a clear reason below the floor.
#[cfg(target_os = "macos")]
fn resolve_macos() -> Loopback {
    match macos_product_version() {
        Some((major, minor)) if (major, minor) >= (14, 6) => {
            Loopback::Available(DeviceSpec::LoopbackDefaultOutput)
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
