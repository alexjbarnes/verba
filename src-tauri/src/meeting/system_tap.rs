//! Global CoreAudio process tap for macOS system-audio capture.
//!
//! This replaces cpal's loopback (a Core Audio process tap bound to one
//! specific OUTPUT device UID) for Meeting mode's system-audio path. A
//! device-bound tap silently delivers silence for some outputs — notably
//! Bluetooth — because it taps a particular device's render path. A GLOBAL
//! tap instead captures all process audio pre-routing, mixed down to mono,
//! independent of whatever output device happens to be active. See
//! `meeting/loopback.rs::resolve_macos`, which selects this path on macOS.
//!
//! Structure mirrors a known-working reference implementation (the Meetily
//! app's `core_audio.rs`): default output device only to park/name the
//! aggregate device -> mono global `CATapDescription` -> aggregate device
//! wrapping just that tap (no sub-device, which would duplicate audio) -> an
//! IO block that hands f32 samples to the rest of the pipeline. The
//! ringbuf/futures `Stream` machinery from that reference is replaced here
//! with a plain `mpsc::Sender`, matching how `AudioRecorder`'s cpal path
//! already feeds `record_loop` (see recorder.rs).
//!
//! Implemented with the pure-Rust `objc2` family of crates (`objc2-core-audio`,
//! `objc2-core-audio-types`, `objc2-core-foundation`, `objc2-foundation`) rather
//! than `cidre`. These are plain header-translator output with no native build
//! step (no `xcodebuild`), so the crate builds with Xcode Command Line Tools
//! alone. All of the CoreAudio C entry points, ObjC classes (`CATapDescription`)
//! and aggregate-device dictionary key constants used below come directly from
//! `objc2-core-audio`'s generated bindings — nothing here is hand-declared FFI.

use std::cell::Cell;
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::sync::mpsc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey, kAudioTapPropertyFormat, AudioDeviceCreateIOProcIDWithBlock,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSUUID};

/// The exact closure signature CoreAudio invokes an `AudioDeviceIOBlock` with:
/// (now, input data, input time, output data, output time). Named so the
/// `RcBlock` we build and the `AudioDeviceIOBlock` FFI parameter it is handed
/// to are provably the same type, with no pointer casting required.
type IoBlockFn = dyn Fn(
    NonNull<AudioTimeStamp>,
    NonNull<AudioBufferList>,
    NonNull<AudioTimeStamp>,
    NonNull<AudioBufferList>,
    NonNull<AudioTimeStamp>,
);

/// Keep-alive handle for a running global system-audio tap. Dropping it
/// tears the capture down.
///
/// Field declaration order does not itself drive cleanup order here (unlike
/// the previous cidre-backed RAII types) — `Drop` below explicitly stops the
/// aggregate device before destroying anything it or its IO block reference.
pub struct SystemTapHandle {
    agg_device_id: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    tap_id: AudioObjectID,
    // Kept alive defensively for the lifetime of the tap. CoreAudio retains
    // its own copy of the block (`Block_copy`) once passed to
    // `AudioDeviceCreateIOProcIDWithBlock`, but there's no reason to drop our
    // reference early.
    _io_block: RcBlock<IoBlockFn>,
}

impl Drop for SystemTapHandle {
    fn drop(&mut self) {
        log::info!("system-tap: stopping global system-audio tap");
        // SAFETY: `agg_device_id`/`proc_id`/`tap_id` were produced by the
        // matching `AudioHardwareCreate*`/`AudioDeviceCreateIOProcIDWithBlock`
        // calls in `start_global_tap` and have not been torn down yet (this
        // is the only place that does so). Order matters: the device must
        // stop producing IO callbacks before the IO proc and the aggregate
        // device it belongs to are destroyed, which in turn must happen
        // before the underlying process tap is destroyed.
        unsafe {
            let status = AudioDeviceStop(self.agg_device_id, self.proc_id);
            if status != 0 {
                log::warn!("system-tap: AudioDeviceStop failed: status {status}");
            }
            let status = AudioDeviceDestroyIOProcID(self.agg_device_id, self.proc_id);
            if status != 0 {
                log::warn!("system-tap: AudioDeviceDestroyIOProcID failed: status {status}");
            }
            let status = AudioHardwareDestroyAggregateDevice(self.agg_device_id);
            if status != 0 {
                log::warn!("system-tap: AudioHardwareDestroyAggregateDevice failed: status {status}");
            }
            let status = AudioHardwareDestroyProcessTap(self.tap_id);
            if status != 0 {
                log::warn!("system-tap: AudioHardwareDestroyProcessTap failed: status {status}");
            }
        }
    }
}

/// Fetch a plain-old-data property (a scalar or a `#[repr(C)]` struct) via
/// `AudioObjectGetPropertyData`, addressed with global scope / main element
/// (every property this module reads is scoped that way).
///
/// # Safety
/// `T` must match the real size/layout of the data the given `selector`
/// returns, or CoreAudio will read/write past the bounds of `T`.
unsafe fn get_property<T>(object_id: AudioObjectID, selector: u32) -> Result<T, String> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = std::mem::size_of::<T>() as u32;
    let mut data = MaybeUninit::<T>::uninit();
    let status = AudioObjectGetPropertyData(
        object_id,
        NonNull::from(&address),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::from(&mut data).cast(),
    );
    if status != 0 {
        return Err(format!(
            "AudioObjectGetPropertyData(selector {selector:#x}) failed: status {status}"
        ));
    }
    Ok(data.assume_init())
}

/// Fetch a CFString-valued property (e.g. a device UID). CoreAudio follows
/// the CF "Get Rule" for these: the returned string already has a +1 retain
/// count that we own.
///
/// # Safety
/// `selector` must name a property whose value is a `CFStringRef`.
unsafe fn get_property_cfstring(
    object_id: AudioObjectID,
    selector: u32,
) -> Result<CFRetained<CFString>, String> {
    let ptr: *const CFString = get_property(object_id, selector)?;
    let ptr = NonNull::new(ptr.cast_mut())
        .ok_or_else(|| format!("AudioObjectGetPropertyData(selector {selector:#x}) returned null"))?;
    Ok(CFRetained::from_raw(ptr))
}

/// Convert one of `objc2_core_audio`'s `&'static CStr` key constants (e.g.
/// `kAudioAggregateDeviceUIDKey`) into the `CFString` needed as an
/// aggregate-device / sub-tap dictionary key.
fn cf_key(s: &'static CStr) -> CFRetained<CFString> {
    CFString::from_str(s.to_str().expect("CoreAudio key constants are ASCII"))
}

/// Start a global system-audio tap (needs macOS 14.4+ for the permission
/// prompt to behave; Meeting mode itself gates on 14.6+, see
/// `meeting/loopback.rs`). Pushes MONO f32 sample chunks to `tx` as they
/// arrive from the CoreAudio IO thread. Returns the keep-alive handle plus
/// the tap's native sample rate (Hz) and channel count.
pub fn start_global_tap(
    tx: mpsc::Sender<Vec<f32>>,
) -> Result<(SystemTapHandle, u32, usize), String> {
    // SAFETY: this function is the sole place that drives the CoreAudio
    // process-tap / aggregate-device lifecycle for a given tap; every value
    // it reads back was just produced by the preceding call, and every
    // partial-failure path below tears down whatever was already created
    // before returning `Err`.
    unsafe {
        // The tap itself captures ALL process audio regardless of output
        // device; the default output device is only used to park/name the
        // aggregate device (CoreAudio requires every aggregate to have one).
        let output_device_id: AudioObjectID = get_property(
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyDefaultOutputDevice,
        )
        .map_err(|e| format!("failed to get default output device: {e}"))?;
        let output_uid = get_property_cfstring(output_device_id, kAudioDevicePropertyDeviceUID)
            .map_err(|e| format!("failed to get default output device UID: {e}"))?;

        // Mono global tap, excluding no processes: mirrors the reference's
        // finding that mono is more reliable for system-audio capture than
        // stereo, and that including a sub-device alongside the tap
        // duplicates audio (echo). The tap alone provides everything we need.
        let exclude: Retained<NSArray<NSNumber>> = NSArray::from_slice(&[]);
        let tap_desc: Retained<CATapDescription> =
            CATapDescription::initMonoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &exclude);
        // Audio is captured by the tap AND still sent to the audio hardware
        // (this is also CATapDescription's default, set explicitly for
        // clarity).
        tap_desc.setMuteBehavior(CATapMuteBehavior::Unmuted);

        let mut tap_id: AudioObjectID = 0;
        let status = AudioHardwareCreateProcessTap(Some(&tap_desc), &mut tap_id);
        if status != 0 {
            return Err(format!("failed to create process tap: status {status}"));
        }

        // Tap UID (for the sub-tap dict) + format (sample rate / channels).
        let tap_uid_string = tap_desc.UUID().to_string();
        let tap_uid_cf = CFString::from_str(&tap_uid_string);

        let asbd: AudioStreamBasicDescription = match get_property(tap_id, kAudioTapPropertyFormat) {
            Ok(asbd) => asbd,
            Err(e) => {
                AudioHardwareDestroyProcessTap(tap_id);
                return Err(format!("failed to get tap audio format: {e}"));
            }
        };
        let sample_rate = asbd.mSampleRate as u32;
        let channels = asbd.mChannelsPerFrame as usize;

        log::info!("system-tap: global tap created, format {sample_rate} Hz / {channels} ch");

        // CRITICAL: only the tap goes into the aggregate (no sub-device-list
        // for the output device) — including both taps the same audio twice.
        let sub_key_uid = cf_key(kAudioSubTapUIDKey);
        let sub_key_drift = cf_key(kAudioSubTapDriftCompensationKey);
        let sub_val_drift = CFBoolean::new(true);
        let sub_keys: [&CFType; 2] = [sub_key_uid.as_ref(), sub_key_drift.as_ref()];
        let sub_values: [&CFType; 2] = [tap_uid_cf.as_ref(), sub_val_drift.as_ref()];
        let sub_tap_dict = CFDictionary::<CFType, CFType>::from_slices(&sub_keys, &sub_values);
        let tap_list = CFArray::<CFDictionary<CFType, CFType>>::from_retained_objects(&[sub_tap_dict]);

        let agg_uid_cf = CFString::from_str(&NSUUID::new().to_string());
        let agg_name_cf = CFString::from_str("verba-system-tap");
        let val_true = CFBoolean::new(true);
        let val_false = CFBoolean::new(false);
        let val_tap_auto_start = CFBoolean::new(true);

        let key_is_private = cf_key(kAudioAggregateDeviceIsPrivateKey);
        let key_is_stacked = cf_key(kAudioAggregateDeviceIsStackedKey);
        let key_tap_auto_start = cf_key(kAudioAggregateDeviceTapAutoStartKey);
        let key_name = cf_key(kAudioAggregateDeviceNameKey);
        let key_main_sub_device = cf_key(kAudioAggregateDeviceMainSubDeviceKey);
        let key_uid = cf_key(kAudioAggregateDeviceUIDKey);
        let key_tap_list = cf_key(kAudioAggregateDeviceTapListKey);

        let keys: [&CFType; 7] = [
            key_is_private.as_ref(),
            key_is_stacked.as_ref(),
            key_tap_auto_start.as_ref(),
            key_name.as_ref(),
            key_main_sub_device.as_ref(),
            key_uid.as_ref(),
            key_tap_list.as_ref(),
        ];
        let values: [&CFType; 7] = [
            val_true.as_ref(),
            val_false.as_ref(),
            val_tap_auto_start.as_ref(),
            agg_name_cf.as_ref(),
            output_uid.as_ref(),
            agg_uid_cf.as_ref(),
            tap_list.as_ref(),
        ];
        let agg_desc = CFDictionary::<CFType, CFType>::from_slices(&keys, &values);

        let mut agg_device_id: AudioObjectID = 0;
        let status = AudioHardwareCreateAggregateDevice(agg_desc.as_opaque(), NonNull::from(&mut agg_device_id));
        if status != 0 {
            AudioHardwareDestroyProcessTap(tap_id);
            return Err(format!("failed to create aggregate device: status {status}"));
        }

        // Diagnostic metering + channel state, moved into the IO block below.
        // `Cell` is enough (no atomics/mutex needed): CoreAudio invokes a
        // given device's IO block serially from one dedicated IO thread.
        let meter_peak = Cell::new(0.0f32);
        let meter_frames = Cell::new(0usize);
        // Opt-in diagnostic level logging, quiet by default. Set
        // VERBA_AUDIO_METER=1 to log this stream's per-second peak amplitude.
        let meter_on = std::env::var_os("VERBA_AUDIO_METER").is_some();

        let io_block: RcBlock<IoBlockFn> = RcBlock::new(
            move |_now: NonNull<AudioTimeStamp>,
                  input_data: NonNull<AudioBufferList>,
                  _input_time: NonNull<AudioTimeStamp>,
                  _output_data: NonNull<AudioBufferList>,
                  _output_time: NonNull<AudioTimeStamp>| {
                // SAFETY: `input_data` is provided by CoreAudio for the
                // duration of this call; for a mono tap it holds exactly one
                // `AudioBuffer` whose `mData` points at `mDataByteSize` bytes
                // of interleaved f32 samples.
                let buffer = input_data.as_ref().mBuffers[0];
                if buffer.mData.is_null() || buffer.mDataByteSize == 0 {
                    return;
                }
                let sample_count = buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
                let samples =
                    std::slice::from_raw_parts(buffer.mData as *const f32, sample_count);

                // Opt-in diagnostic metering (VERBA_AUDIO_METER): peak |sample|
                // + running frame count, logged ~once/sec so the logs alone
                // tell us whether the tap is delivering real audio, pure
                // silence, or nothing at all.
                if meter_on {
                    let mut peak = meter_peak.get();
                    for &s in samples {
                        let a = s.abs();
                        if a > peak {
                            peak = a;
                        }
                    }
                    let frames = meter_frames.get() + samples.len();
                    let rate = sample_rate.max(1) as usize;
                    if frames >= rate {
                        log::info!(
                            "audio level [system-tap]: peak {:.4} over ~{}ms",
                            peak,
                            frames * 1000 / rate
                        );
                        meter_peak.set(0.0);
                        meter_frames.set(0);
                    } else {
                        meter_peak.set(peak);
                        meter_frames.set(frames);
                    }
                }

                let _ = tx.send(samples.to_vec());
            },
        );

        let mut proc_id: AudioDeviceIOProcID = None;
        let status = AudioDeviceCreateIOProcIDWithBlock(
            NonNull::from(&mut proc_id),
            agg_device_id,
            None,
            RcBlock::as_ptr(&io_block),
        );
        if status != 0 {
            AudioHardwareDestroyAggregateDevice(agg_device_id);
            AudioHardwareDestroyProcessTap(tap_id);
            return Err(format!("failed to create IO proc: status {status}"));
        }

        let status = AudioDeviceStart(agg_device_id, proc_id);
        if status != 0 {
            AudioDeviceDestroyIOProcID(agg_device_id, proc_id);
            AudioHardwareDestroyAggregateDevice(agg_device_id);
            AudioHardwareDestroyProcessTap(tap_id);
            return Err(format!("failed to start aggregate device: status {status}"));
        }

        log::info!("system-tap: global system-audio tap started");

        Ok((
            SystemTapHandle {
                agg_device_id,
                proc_id,
                tap_id,
                _io_block: io_block,
            },
            sample_rate,
            channels,
        ))
    }
}
