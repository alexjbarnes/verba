fn main() {
    tauri_build::build();

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // Desktop shared-ORT setup (mirrors Android pattern).
    // When SHERPA_ONNX_LIB_DIR contains a .shared-ort marker, the directory
    // has static sherpa-onnx libs + a stub libonnxruntime.a + the real
    // libonnxruntime.dylib.  sherpa-onnx-sys emits `static=onnxruntime`
    // (satisfied by the stub); we add `dylib=onnxruntime` so the real ORT
    // is loaded as a shared library.  The ort crate's dlopen then finds
    // the already-loaded library — single ORT instance, same as Android.
    if target_os != "android" {
        if let Ok(lib_dir) = std::env::var("SHERPA_ONNX_LIB_DIR") {
            let lib_path = std::path::Path::new(&lib_dir);
            if lib_path.join(".shared-ort").exists() {
                println!("cargo:rustc-link-lib=dylib=onnxruntime");

                // Set rpath so the binary finds the dylib at runtime
                if target_os == "macos" {
                    println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
                    println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
                } else {
                    println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
                    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
                }
            }
        }
        println!("cargo:rerun-if-env-changed=SHERPA_ONNX_LIB_DIR");
    }

    // Compile the SIGABRT guard for desktop Unix (macOS / Linux).
    // Recovers from C++ exceptions that bypass catch_unwind and reach abort().
    if target_os != "android" && (target_os == "macos" || target_os == "linux") {
        cc::Build::new().file("abort_guard.c").compile("abort_guard");
        println!("cargo:rerun-if-changed=abort_guard.c");

        // The host sherpa-onnx prebuilt references espeak symbols from its
        // TTS path (which Verba never uses; our TTS is piper.rs + ort,
        // espeak-free). Stub them so the library links on the host — this is
        // what lets `cargo test --lib` and host bins run at all.
        // The sherpa prebuilt uses the pre-cxx11 std::string ABI; the stub's
        // mangled names must match or the linker keeps looking.
        cc::Build::new()
            .cpp(true)
            .define("_GLIBCXX_USE_CXX11_ABI", "0")
            .file("host_stubs.cpp")
            .compile("host_stubs");
        println!("cargo:rerun-if-changed=host_stubs.cpp");
    }

    if target_os == "android" {
        // sherpa-onnx / ONNX Runtime C++ code requires the C++ runtime.
        // Force the linker to record libc++_shared.so as a NEEDED dependency
        // so the dynamic linker loads it and resolves C++ ABI symbols
        // (__gxx_personality_v0, operator new/delete, etc.) at load time.
        // --no-as-needed prevents the linker from dropping the dep if it
        // thinks all symbols are already resolved from the static lib.
        println!("cargo:rustc-link-arg=-Wl,--no-as-needed,-lc++_shared,--as-needed");
        println!("cargo:rustc-link-lib=dylib=log");

        // sherpa-onnx's session.cc references OrtSessionOptionsAppendExecutionProvider_Nnapi.
        // Keep this stub so the symbol is always defined regardless of ORT build flags.
        cc::Build::new().file("stubs.c").compile("stubs");

        // sherpa-onnx is compiled against the official ORT Android shared library
        // (libonnxruntime.so) rather than a static ORT archive. This means:
        //   1. libverba_rs_lib.so records libonnxruntime.so as DT_NEEDED.
        //   2. Android loads libonnxruntime.so before any user code runs.
        //   3. When the ort Rust crate calls dlopen("libonnxruntime.so"), the OS
        //      returns the already-loaded handle — one ORT instance in the process,
        //      no shared global state corruption, no recorder thread crash.
        //
        // sherpa-onnx-sys emits `static=onnxruntime`, which expects libonnxruntime.a
        // in SHERPA_ONNX_LIB_DIR. android-build.sh --setup-only creates an empty
        // stub archive there so that directive is satisfied without embedding ORT code.
        println!("cargo:rustc-link-lib=dylib=onnxruntime");
        println!("cargo:rerun-if-env-changed=SHERPA_ONNX_LIB_DIR");

        // Copy libonnxruntime.so to jniLibs so the APK includes it and the Android
        // Package Manager extracts it to the app's native library directory at install.
        if let Ok(lib_dir) = std::env::var("SHERPA_ONNX_LIB_DIR") {
            let ort_so = std::path::Path::new(&lib_dir).join("libonnxruntime.so");
            let jni_libs = std::path::Path::new(&manifest_dir)
                .join("gen/android/app/src/main/jniLibs/arm64-v8a");
            if ort_so.exists() {
                let _ = std::fs::create_dir_all(&jni_libs);
                let _ = std::fs::copy(&ort_so, jni_libs.join("libonnxruntime.so"));
            }
        }
    }
}
