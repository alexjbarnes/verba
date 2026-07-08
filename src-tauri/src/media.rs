use std::sync::atomic::{AtomicBool, Ordering};

/// Whether we paused media playback and should resume it later.
static WAS_PLAYING: AtomicBool = AtomicBool::new(false);

/// Request other apps to pause media playback (e.g. music, podcasts)
/// so audio doesn't bleed into the microphone during dictation.
pub fn pause_media() {
    WAS_PLAYING.store(false, Ordering::SeqCst);
    if platform::do_pause() {
        WAS_PLAYING.store(true, Ordering::SeqCst);
    }
}

/// Resume media playback if we previously paused it.
pub fn resume_media() {
    if WAS_PLAYING.swap(false, Ordering::SeqCst) {
        platform::do_resume();
    }
}

#[cfg(target_os = "android")]
mod platform {
    pub fn do_pause() -> bool {
        call_audio_focus("requestAudioFocus")
    }

    pub fn do_resume() {
        call_audio_focus("abandonAudioFocus");
    }

    fn call_audio_focus(method: &str) -> bool {
        let vm = match crate::GLOBAL_JVM.get() {
            Some(v) => v,
            None => { log::warn!("media: JVM not initialized"); return false; }
        };
        let class_ref = match crate::VERBA_APP_CLASS.get() {
            Some(c) => c,
            None => { log::warn!("media: VerbaApp class not cached"); return false; }
        };
        let mut env = match vm.attach_current_thread_permanently() {
            Ok(e) => e,
            Err(e) => { log::warn!("media: attach: {e}"); return false; }
        };
        let class = unsafe { jni::objects::JClass::from_raw(class_ref.as_raw()) };
        match env.call_static_method(class, method, "()Z", &[]) {
            Ok(ret) => ret.z().unwrap_or(false),
            Err(e) => {
                log::warn!("media: {method}: {e}");
                false
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_void, CStr};
    use std::sync::OnceLock;

    type MRMediaRemoteSendCommandFn = unsafe extern "C" fn(u32, *mut c_void) -> bool;
    type MRMediaRemoteGetNowPlayingInfoFn =
        unsafe extern "C" fn(dispatch_queue: *mut c_void, callback: *mut c_void);

    #[repr(C)]
    struct dispatch_queue_s {
        _opaque: [u8; 0],
    }

    extern "C" {
        fn dispatch_queue_create(
            label: *const i8,
            attr: *const c_void,
        ) -> *mut dispatch_queue_s;
    }

    struct MediaRemote {
        send_command: MRMediaRemoteSendCommandFn,
        get_now_playing_info: MRMediaRemoteGetNowPlayingInfoFn,
    }

    const MR_MEDIA_REMOTE_COMMAND_PLAY: u32 = 0;
    const MR_MEDIA_REMOTE_COMMAND_PAUSE: u32 = 2;

    unsafe fn dlsym_fn<T>(lib: *mut c_void, name: &CStr) -> Option<T> {
        let ptr = libc::dlsym(lib, name.as_ptr());
        if ptr.is_null() { None } else { Some(std::mem::transmute_copy(&ptr)) }
    }

    fn load() -> Option<&'static MediaRemote> {
        static INSTANCE: OnceLock<Option<MediaRemote>> = OnceLock::new();
        INSTANCE.get_or_init(|| unsafe {
            let mr = libc::dlopen(
                c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote".as_ptr(),
                libc::RTLD_LAZY,
            );
            if mr.is_null() { return None; }
            Some(MediaRemote {
                send_command: dlsym_fn(mr, c"MRMediaRemoteSendCommand")?,
                get_now_playing_info: dlsym_fn(mr, c"MRMediaRemoteGetNowPlayingInfo")?,
            })
        }).as_ref()
    }

    fn is_media_playing() -> bool {
        use core_foundation::base::TCFType;
        use core_foundation::dictionary::CFDictionaryRef;
        use core_foundation::number::CFNumber;
        use core_foundation::string::CFString;
        use std::sync::{Condvar, Mutex};
        use std::time::Duration;

        let mr = match load() {
            Some(m) => m,
            None => return false,
        };

        let pair = std::sync::Arc::new((Mutex::new(None::<bool>), Condvar::new()));
        let pair_clone = pair.clone();

        let block = block2::RcBlock::new(move |info: *mut c_void| {
            let info = info as CFDictionaryRef;
            let playing = if info.is_null() {
                false
            } else {
                unsafe {
                    let key = CFString::new("kMRMediaRemoteNowPlayingInfoPlaybackRate");
                    let mut value: *const c_void = std::ptr::null();
                    let found = core_foundation::dictionary::CFDictionaryGetValueIfPresent(
                        info,
                        key.as_concrete_TypeRef() as *const c_void,
                        &mut value,
                    );
                    if found != 0 && !value.is_null() {
                        let num = CFNumber::wrap_under_get_rule(value as *const _);
                        let rate: f64 = num.to_f64().unwrap_or(0.0);
                        rate > 0.5
                    } else {
                        false
                    }
                }
            };
            let (lock, cvar) = &*pair_clone;
            *lock.lock().unwrap() = Some(playing);
            cvar.notify_one();
        });

        unsafe {
            let queue = dispatch_queue_create(
                c"com.verba.media_check".as_ptr(),
                std::ptr::null(),
            );
            (mr.get_now_playing_info)(
                queue as *mut c_void,
                block2::RcBlock::as_ptr(&block) as *mut c_void,
            );
        }

        let (lock, cvar) = &*pair;
        let guard = lock.lock().unwrap();
        let result = cvar
            .wait_timeout_while(guard, Duration::from_millis(100), |v| v.is_none())
            .ok()
            .and_then(|(g, _)| *g)
            .unwrap_or(false);

        log::debug!("media: NowPlaying is_playing={result}");
        result
    }

    pub fn do_pause() -> bool {
        if !is_media_playing() {
            log::debug!("media: nothing playing, skip pause");
            return false;
        }
        let mr = match load() {
            Some(m) => m,
            None => return false,
        };
        unsafe { (mr.send_command)(MR_MEDIA_REMOTE_COMMAND_PAUSE, std::ptr::null_mut()) }
    }

    pub fn do_resume() {
        let mr = match load() {
            Some(m) => m,
            None => return,
        };
        unsafe { (mr.send_command)(MR_MEDIA_REMOTE_COMMAND_PLAY, std::ptr::null_mut()); }
    }
}

#[cfg(not(any(target_os = "android", target_os = "macos")))]
mod platform {
    pub fn do_pause() -> bool {
        false
    }

    pub fn do_resume() {}
}
