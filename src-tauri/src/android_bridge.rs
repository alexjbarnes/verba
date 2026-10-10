//! JNI bridge to VerbaApp.kt for the reading app: clipboard, hidden-WebView
//! fetch fallback, and the Android share-target entry point.

use std::sync::OnceLock;

static GLOBAL_JVM: OnceLock<jni::JavaVM> = OnceLock::new();

/// GlobalRef to VerbaApp class, cached at JNI_OnLoad time.
///
/// Android's class loader is thread-local. Background Rust threads that attach to
/// the JVM via attach_current_thread get the bootstrap class loader, which only
/// knows SDK classes — it cannot find app classes like VerbaApp. By resolving and
/// caching the class during JNI_OnLoad (called on the Java main thread, which has
/// the application class loader), we can safely use it from any thread afterwards.
static VERBA_APP_CLASS: OnceLock<jni::objects::GlobalRef> = OnceLock::new();

/// Called once by the Android runtime when System.loadLibrary("verba_rs_lib") runs.
/// We are on the Java main thread here, so find_class works for app classes.
#[no_mangle]
pub extern "system" fn JNI_OnLoad(
    vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> jni::sys::jint {
    let Ok(vm) = (unsafe { jni::JavaVM::from_raw(vm) }) else {
        return jni::sys::JNI_VERSION_1_6;
    };
    if let Ok(mut env) = vm.get_env() {
        if let Ok(class) = env.find_class("com/alexb151/verba/VerbaApp") {
            if let Ok(global) = env.new_global_ref(class) {
                let _ = VERBA_APP_CLASS.set(global);
            }
        }
    }
    let _ = GLOBAL_JVM.set(vm);
    jni::sys::JNI_VERSION_1_6
}

pub fn copy_to_clipboard(text: &str) {
    use jni::objects::JValue;
    let Some(vm) = GLOBAL_JVM.get() else { return };
    let Some(class_ref) = VERBA_APP_CLASS.get() else { return };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(e) => e,
        Err(_) => return,
    };
    let Ok(text_str) = env.new_string(text) else { return };
    let class = unsafe { jni::objects::JClass::from_raw(class_ref.as_raw()) };
    let _ = env.call_static_method(
        class, "copyToClipboard", "(Ljava/lang/String;)V",
        &[JValue::Object(&*text_str)],
    );
}

/// Open the system share sheet for a URL (Android ACTION_SEND).
pub fn share_text(text: &str) {
    use jni::objects::JValue;
    let Some(vm) = GLOBAL_JVM.get() else { return };
    let Some(class_ref) = VERBA_APP_CLASS.get() else { return };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(e) => e,
        Err(_) => return,
    };
    let Ok(text_str) = env.new_string(text) else { return };
    let class = unsafe { jni::objects::JClass::from_raw(class_ref.as_raw()) };
    let _ = env.call_static_method(
        class, "shareText", "(Ljava/lang/String;)V",
        &[JValue::Object(&*text_str)],
    );
}

/// Ask Kotlin to load a URL in a hidden WebView (challenge-passing fallback
/// fetch). The result comes back asynchronously via nativeWebFetchDone.
pub fn webview_fetch(url: &str, request_id: i64) {
    use jni::objects::JValue;
    let Some(vm) = GLOBAL_JVM.get() else {
        return crate::webfetch::complete(request_id, Err("JVM not ready".into()));
    };
    let Some(class_ref) = VERBA_APP_CLASS.get() else {
        return crate::webfetch::complete(request_id, Err("app class not ready".into()));
    };
    let mut env = match vm.attach_current_thread_permanently() {
        Ok(e) => e,
        Err(e) => return crate::webfetch::complete(request_id, Err(format!("JNI attach: {e}"))),
    };
    let Ok(url_str) = env.new_string(url) else {
        return crate::webfetch::complete(request_id, Err("JNI string alloc failed".into()));
    };
    let class = unsafe { jni::objects::JClass::from_raw(class_ref.as_raw()) };
    if env
        .call_static_method(
            class,
            "webViewFetch",
            "(Ljava/lang/String;J)V",
            &[JValue::Object(&*url_str), JValue::Long(request_id)],
        )
        .is_err()
    {
        crate::webfetch::complete(request_id, Err("webViewFetch call failed".into()));
    }
}

/// Android share-target bridge: MainActivity forwards an ACTION_SEND intent's
/// text here.
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeSharedText(
    mut env: jni::JNIEnv, _class: jni::objects::JClass, text: jni::objects::JString,
) {
    match env.get_string(&text) {
        Ok(s) => crate::share::push_shared_text(s.into()),
        Err(e) => log::error!("share: failed to read shared text: {e}"),
    }
}

/// Hidden-WebView fetch completion: Kotlin hands back the page body (or an
/// error) for the request id issued by webfetch::fetch.
#[no_mangle]
pub extern "system" fn Java_com_alexb151_verba_VerbaApp_nativeWebFetchDone(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    request_id: jni::sys::jlong,
    content: jni::objects::JString,
    error: jni::objects::JString,
) {
    let content: String = env.get_string(&content).map(Into::into).unwrap_or_default();
    let error: String = env.get_string(&error).map(Into::into).unwrap_or_default();
    let result = if error.is_empty() {
        Ok(content)
    } else {
        Err(error)
    };
    crate::webfetch::complete(request_id, result);
}
