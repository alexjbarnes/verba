# Android Cross-Compilation: Findings and Gotchas

Hard-won lessons from porting Verba (Tauri v2 + Rust + sherpa-onnx) to Android arm64.
Written March 2026.

## Quick Start

Build a signed APK with one command:

```
just apk
```

Output: `verba.apk` in the repo root (~51MB, arm64 only).

Other commands:

```
just apk-release   # release APK
just test           # run Rust library tests
just check          # fast compile check (no linking)
just clean          # nuke all build artifacts
```

The justfile handles everything: native Rust compilation via cargo-ndk, stripping debug
symbols, copying frontend assets, Gradle packaging, and signing with a persistent debug
keystore.

## Prerequisites

- Android SDK with platform 34+ (`ANDROID_HOME`)
- Android NDK r28+ (`ANDROID_NDK_HOME`)
- JDK 17+ (`JAVA_HOME`)
- Rust aarch64-linux-android target: `rustup target add aarch64-linux-android`
- cargo-ndk: `cargo install cargo-ndk`
- cmake, ninja-build (for sherpa-onnx native libs)
- Pre-built sherpa-onnx static libraries at `.android-deps/sherpa-onnx/install/lib/`

Environment variables are auto-detected by the justfile from standard locations. Override
with `ANDROID_HOME`, `ANDROID_NDK_HOME`, `JAVA_HOME` env vars if your setup differs.

NDK 28+ is required for Google Play's 16KB page alignment. JDK 17 works with Gradle 8.x.

## First-Time Setup

sherpa-onnx native libraries must be built from source once. The `scripts/android-build.sh`
script handles this:

```
./scripts/android-build.sh --setup-only
```

This clones sherpa-onnx, cross-compiles for arm64-v8a via CMake, and caches the static
libraries in `.android-deps/sherpa-onnx/install/lib/`. Takes 10-15 minutes. After that,
`just apk` works without repeating this step.

## Build Pipeline Details

The `just apk` recipe runs these steps in order:

1. **Keystore** - generates `debug.keystore` in repo root on first run (gitignored)
2. **Tauri CLI** - `npx tauri android build --target aarch64 --apk` handles Rust compilation,
   frontend bundling, and Gradle packaging in one step
3. **Strip** - removes debug symbols with llvm-strip (80MB -> 40MB), removes stale x86_64 stubs
4. **Sign** - zipalign + apksigner with the debug keystore

### Why use `npx tauri android build`?

The Tauri CLI sets critical environment variables (`TAURI_ENV_TARGET_TRIPLE`,
`WRY_ANDROID_PACKAGE`, etc.) during Rust compilation that affect how the Android WebView
serves frontend assets at runtime. Building with `cargo ndk` directly bypasses this setup,
which causes the WebView to fail with "Failed to request http://tauri.localhost/".

The justfile adds stripping and custom signing as post-build steps on top of the Tauri CLI
build.

### Signing

The debug keystore lives at `debug.keystore` in the repo root (gitignored). It persists
across builds so Android accepts APK upgrades without uninstalling. If you lose it, you need
to uninstall the app on the device before installing a new APK signed with a different key.

Password: `android` (standard Android debug convention).

## The Dynamic Linker Problem

Android's dynamic linker (`linker64`) is strict about symbol resolution at library load time.
On Linux desktop, GLOBAL UNDEFINED symbols in a `.so` might never be called and are harmless.
On Android arm64, any GLOBAL UNDEFINED symbol that can't be resolved causes an immediate crash
before any of your code runs. No error message in your app, no chance to catch it. The process
just dies.

### How to diagnose

Use NDK tools to inspect the compiled `.so`:

```
NDK=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
$NDK/llvm-readelf -r libscribe_rs_lib.so | grep NNAPI
$NDK/llvm-nm -D libscribe_rs_lib.so | grep " U "
```

Look for `U` (undefined) symbols that reference APIs not available on the device.

### Specific case: sherpa-onnx NNAPI symbol

sherpa-onnx's `session.cc` references `OrtSessionOptionsAppendExecutionProvider_Nnapi`. This
symbol comes from the ONNX Runtime NNAPI execution provider, which isn't included in the
pre-built static library. On desktop this is fine because the symbol is never called. On
Android arm64, the dynamic linker sees it as unresolvable and kills the process at load time.

Fix: provide a C stub that satisfies the linker:

```c
// stubs.c
typedef struct OrtStatus OrtStatus;
typedef struct OrtSessionOptions OrtSessionOptions;

OrtStatus* OrtSessionOptionsAppendExecutionProvider_Nnapi(
    OrtSessionOptions* options, unsigned int nnapi_flags) {
    (void)options;
    (void)nnapi_flags;
    return (OrtStatus*)0;  // null = success in ORT convention
}
```

Compile it in `build.rs`:

```rust
let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
if target_os == "android" {
    cc::Build::new().file("stubs.c").compile("stubs");
}
```

Returning null (success) is safe as long as you also set the execution provider to CPU
explicitly in your model config. sherpa-onnx's session.cc checks the return value and falls
back to CPU on error, but explicitly setting `provider = "cpu"` avoids depending on that
behavior.

### Transitive symbol pollution

This is the subtlest gotcha. Even if your code never calls a function containing the bad
symbol, linking against the static archive that contains it can still pull in the object file.

Example: `sherpa_onnx::LinearResampler` lives in `sherpa-onnx-core`. This static library also
contains `session.cc.o`, which has the NNAPI reference. When the linker resolves
`LinearResampler`, it pulls in the entire object file graph, including `session.cc.o` and its
NNAPI dependency.

We discovered this through binary search: cpal alone loaded fine, cpal + `build_input_stream`
loaded fine, but cpal + `LinearResampler` crashed. The resampler itself had nothing to do with
NNAPI, but it shared a static archive with code that did.

Fix: replace `sherpa_onnx::LinearResampler` with a pure Rust implementation. Linear
interpolation resampling is straightforward and avoids the transitive dependency entirely. See
`src-tauri/src/recorder.rs` for the implementation.

General principle: on Android, every function you call from a static C/C++ archive may drag
in unrelated object files with unresolvable symbols. The only way to know is to build, inspect
with `llvm-nm`, and test on a real device.

## C++ Standard Library Linking

Android NDK provides `libc++` (LLVM's C++ runtime). Many native dependencies need it.

### The dual-linking problem

If two native deps link against different C++ runtimes, you get ODR violations, mysterious
crashes, or link failures. In our case:

- cpal uses oboe, which statically links `c++_static`
- sherpa-onnx / ONNX Runtime needs `c++_shared`

Both coexist in the final `.so`. This works in practice because `c++_static` symbols are
hidden (local), but it's fragile. If you add more C++ deps, audit which runtime they use.

### Forcing libc++_shared

sherpa-onnx needs the C++ runtime's symbols at load time. Force the linker to record the
dependency even if it thinks all symbols resolve statically:

```rust
// build.rs
if target_os == "android" {
    println!("cargo:rustc-link-arg=-Wl,--no-as-needed,-lc++_shared,--as-needed");
}
```

Without `--no-as-needed`, the linker may optimize away the `libc++_shared.so` NEEDED entry,
causing C++ ABI symbols (`__gxx_personality_v0`, `operator new`, etc.) to be missing at
runtime.

### Android log library

Native code on Android needs `liblog.so` for `__android_log_write` and friends:

```rust
println!("cargo:rustc-link-lib=dylib=log");
```

## Native Crashes and Process Isolation

### The problem

Native crashes (SIGSEGV, SIGABRT) from C/C++ code kill the entire process. Rust's
`catch_unwind` only catches Rust panics. A bad pointer dereference in ONNX Runtime, a null
deref in sherpa-onnx, or a memory corruption in any C dep takes out the app with no chance
to recover.

On desktop, this is annoying. On Android, it means the app disappears with no error message
and the user has no idea what happened.

### fork() as process isolation

The only reliable way to survive native crashes is process isolation. On Android (Linux),
`fork()` creates a child process that shares the parent's memory (copy-on-write). If the child
crashes, the parent survives.

Pattern:

1. Parent creates a pipe
2. Parent forks
3. Child closes the read end of the pipe
4. Child loads the model, runs inference, writes result to pipe
5. Parent closes the write end, reads result from pipe
6. Parent calls `waitpid` to collect the child's exit status

If the child crashes (signal), the pipe breaks and the parent reads an empty buffer. The
parent can then inspect the exit status via `WIFSIGNALED` / `WTERMSIG` to report which signal
killed the child.

See `src-tauri/src/transcribe.rs` for the full implementation. Key details:

- Use `libc::pipe`, `libc::fork`, `libc::waitpid` directly. The `nix` crate is an option but
  `libc` is already a dependency.
- The child must call `libc::_exit(0)`, not `std::process::exit()`. The Rust exit function
  runs destructors and flushes stdio, which can deadlock after fork.
- Protocol over the pipe: `OK:text` for success, `ERR:message` for errors. Empty buffer means
  the child crashed before writing anything.
- A mutex in the parent prevents concurrent fork+inference calls.

### Desktop fallback

fork() is Linux-only. On macOS and Windows, run inference on a separate thread instead.
Thread crashes still kill the app, but desktop ONNX Runtime is more stable (no NNAPI
weirdness, mature x86_64 codepath). Use `#[cfg(target_os = "android")]` to select the path:

```rust
pub fn transcribe(&self, samples: Vec<f32>, sample_rate: i32) -> Result<String, String> {
    #[cfg(not(target_os = "android"))]
    {
        return self.transcribe_in_process(&samples, sample_rate);
    }
    #[cfg(target_os = "android")]
    {
        self.transcribe_in_fork(&samples, sample_rate)
    }
}
```

### Surfacing errors to the UI

Native crashes become recoverable errors via the fork pattern, but they still need to reach
the user. Emit events from Rust to the frontend:

```rust
fn emit_error(&self, msg: &str) {
    log::error!("{msg}");
    if let Some(ref handle) = *self.app_handle.lock().unwrap() {
        let _ = handle.emit("dictation-error", serde_json::json!({ "error": msg }));
    }
    self.emit_state("error");
}
```

Frontend listens for these:

```javascript
listen('dictation-error', (event) => {
    recordStatus.textContent = 'Error: ' + event.payload.error;
});
```

## Lazy Initialization on Android

Desktop can load models eagerly at startup. Android should defer initialization until first
use for two reasons:

1. Model loading is slow and blocks the UI thread if done at startup
2. Some native code crashes at load time, and lazy init lets the app at least start

Pattern: `ensure_recorder()` and `ensure_transcriber()` methods that check an
`Option<T>` behind a mutex and initialize on first call. The Tauri command handler calls both
before starting dictation:

```rust
#[tauri::command]
async fn start_dictation(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<DictationManager>>,
) -> Result<(), String> {
    let dm = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        dm.ensure_recorder();
        dm.ensure_transcriber(&app);
        dm.start();
    })
    .await
    .map_err(|e| format!("join error: {e}"))
}
```

## Conditional Compilation Patterns

Use `cfg` attributes to separate desktop and mobile code paths:

```toml
# Cargo.toml - deps only needed on desktop
[target.'cfg(not(target_os = "android"))'.dependencies]
arboard = "3"
enigo = "0.3"
tauri-plugin-global-shortcut = "2"
```

```rust
// Code that only runs on desktop
#[cfg(desktop)]
crate::sound::start_beep();

// Android-specific code
#[cfg(target_os = "android")]
self.transcribe_in_fork(&samples, sample_rate)
```

Tauri provides `#[cfg(desktop)]` and `#[cfg(mobile)]` as convenience aliases.

## Debugging Tips

### Reading logs from the device

```
adb logcat -s RustStdoutStderr:V
adb logcat | grep -i "verba\|sherpa\|onnx\|crash\|signal"
```

### Inspecting the built .so

```
NDK=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin

# List all undefined symbols
$NDK/llvm-nm -D target/aarch64-linux-android/release/libscribe_rs_lib.so | grep " U "

# Check NEEDED shared libraries
$NDK/llvm-readelf -d target/aarch64-linux-android/release/libscribe_rs_lib.so | grep NEEDED

# Check for init_array entries (C++ global constructors that run at load time)
$NDK/llvm-readelf -S target/aarch64-linux-android/release/libscribe_rs_lib.so | grep init_array

# Size of the .so
ls -lh target/aarch64-linux-android/release/libscribe_rs_lib.so
```

### Binary search for symbol pollution

When you can't figure out which code path is pulling in a bad symbol:

1. Comment out code paths one at a time
2. Build and inspect with `llvm-nm` after each change
3. When the bad symbol disappears, you found the guilty path

We used this to isolate that `sherpa_onnx::LinearResampler` (not the transcriber) was pulling
in `session.cc.o`. The transcriber was already behind a lazy init gate and wasn't the problem.

### Test on real hardware

Emulators run x86_64, not arm64. ONNX Runtime behavior, NEON optimizations, and dynamic
linker strictness differ. Always test on a physical arm64 device.

## Known Issues (as of March 2026)

### ONNX Runtime inference crashes on arm64

The arm64 build of ONNX Runtime (via sherpa-onnx pre-built libs) crashes during inference
for the Parakeet TDT 0.6B model. The model loads successfully (`OfflineRecognizer::create`
returns `Some`), but `recognizer.decode()` triggers a native crash (SIGSEGV or SIGABRT).

This might be a model compatibility issue with the specific ORT build, an INT8 quantization
issue on arm64, or a memory alignment problem. The fork-based transcriber isolates this crash
so the app survives, but transcription doesn't work yet.

Potential next steps:
- Try a different model (Whisper Tiny via sherpa-onnx, smaller and more widely tested on arm64)
- Try FP16 instead of INT8 quantization
- Build sherpa-onnx from source with debug symbols to get a stack trace from the crash
- Check if the ORT build was compiled with XNNPACK (arm64 SIMD acceleration)

### Pure Rust ASR not viable on Android

See architecture.md for benchmarks. candle Whisper is 3x slower than real-time on Pixel 7.
tract cannot run Parakeet. sherpa-onnx is the only option that achieves real-time ASR on
Android, which is why we deal with all the C++ linking complexity.

### Dual C++ runtime

The final binary links both `c++_static` (from oboe/cpal) and `c++_shared` (from
sherpa-onnx/ORT). This works because the static symbols are local, but it's not ideal. If
you add more C++ deps, verify they don't introduce ABI conflicts. Run
`llvm-nm -D | grep c++` to check.
