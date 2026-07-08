# Verba build automation
#
# Usage:
#   just setup         # first-time desktop setup: shared-ORT libs + grammar models
#   just dev           # fast desktop dev loop (hot-reload; NO system-audio capture)
#   just desktop       # build + run the signed .app (Meeting system-audio works)
#   just setup-android # first-time Android setup: sherpa-onnx, ORT, grammar models
#   just apk           # build debug APK
#   just apk-release   # build release APK
#   just test          # run Rust tests
#   just check         # cargo check
#   just eval          # run pipeline eval harness against test cases
#   just clean         # clean all build artifacts

# ── Configuration ──

repo_root    := justfile_directory()
tauri_dir    := repo_root / "src-tauri"
android_dir  := tauri_dir / "gen" / "android"
jni_dir      := android_dir / "app" / "src" / "main" / "jniLibs"
keystore     := repo_root / "debug.keystore"
desktop_deps := repo_root / ".desktop-deps"

# sherpa-onnx version must match Cargo.toml's sherpa-onnx dependency
sherpa_version := "1.12.34"

# Auto-detect paths
android_home := env("ANDROID_HOME", `echo ${HOME}/Android/Sdk`)
android_ndk  := env("ANDROID_NDK_HOME", `ls -1d ${ANDROID_HOME:-$HOME/Android/Sdk}/ndk/* 2>/dev/null | sort -V | tail -1 || echo ""`)
build_tools  := `ls -1d ${ANDROID_HOME:-$HOME/Android/Sdk}/build-tools/* 2>/dev/null | sort -V | tail -1 || echo ""`
sherpa_libs  := repo_root / ".android-deps" / "sherpa-onnx" / "install" / "lib"
strip_bin    := android_ndk / "toolchains" / "llvm" / "prebuilt" / "linux-x86_64" / "bin" / "llvm-strip"

export ANDROID_HOME := android_home
export ANDROID_NDK_HOME := android_ndk
export JAVA_HOME := env("JAVA_HOME", "/usr")

# ── Recipes ──

# First-time desktop setup: shared-ORT sherpa-onnx libs + grammar models
setup: _setup-sherpa-desktop _setup-grammar

# Bare binary: macOS Meeting-mode system-audio capture does NOT work here
# (CoreAudio taps are gated to signed bundled apps launched via LaunchServices —
# use `just desktop` for that).
# Fast desktop dev loop: hot-reload, shared-ORT env (no Meeting system-audio).
dev:
    SHERPA_ONNX_LIB_DIR="{{desktop_deps}}/sherpa-onnx/lib" npx tauri dev

# Rebuilds the debug bundle, re-signs it ad-hoc with tauri.conf.json's
# entitlements, and launches via LaunchServices (logs -> /tmp/verba-desktop.log).
# No hot-reload — re-run after code changes.
# Build + run the signed desktop .app — Meeting-mode system-audio works here.
desktop:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "==> Building + signing debug .app bundle (first build takes a while)..."
    SHERPA_ONNX_LIB_DIR="{{desktop_deps}}/sherpa-onnx/lib" npx tauri build --debug --bundles app
    APP="{{tauri_dir}}/target/debug/bundle/macos/Verba.app"
    LOG="/tmp/verba-desktop.log"
    : > "$LOG"
    # Replace any previous instance so the fresh build is what runs.
    pkill -f "Verba.app/Contents/MacOS/verba-rs" 2>/dev/null || true
    open "$APP" --stdout "$LOG" --stderr "$LOG"
    echo ""
    echo "==> Launched $APP"
    echo "    logs:  tail -f $LOG"
    echo "    On the first meeting, macOS prompts to record system audio — click Allow."

# ── Desktop setup internals ──

# Download sherpa-onnx prebuilt libs and set up shared ORT (mirrors Android pattern).
# Static sherpa-onnx libs + stub libonnxruntime.a + real libonnxruntime.dylib
# so both sherpa-onnx and the ort crate share a single ORT instance.
_setup-sherpa-desktop:
    #!/usr/bin/env bash
    set -euo pipefail
    LIB_DIR="{{desktop_deps}}/sherpa-onnx/lib"
    MARKER="$LIB_DIR/.shared-ort"
    if [ -f "$MARKER" ]; then
        echo "==> Desktop sherpa-onnx libs already prepared"
        exit 0
    fi

    SHERPA_VERSION="{{sherpa_version}}"
    # ORT version must match what sherpa-onnx was built against
    ORT_VERSION="1.24.2"
    CACHE="{{desktop_deps}}/cache"
    mkdir -p "$CACHE" "$LIB_DIR"

    # Detect platform
    ARCH=$(uname -m)
    OS=$(uname -s)
    if [ "$OS" = "Darwin" ] && [ "$ARCH" = "arm64" ]; then
        STATIC_ARCHIVE="sherpa-onnx-v${SHERPA_VERSION}-osx-arm64-static-lib.tar.bz2"
        ORT_ARCHIVE="onnxruntime-osx-arm64-${ORT_VERSION}.tgz"
        ORT_LIBNAME="libonnxruntime.${ORT_VERSION}.dylib"
        ORT_LINK="libonnxruntime.dylib"
    elif [ "$OS" = "Darwin" ] && [ "$ARCH" = "x86_64" ]; then
        STATIC_ARCHIVE="sherpa-onnx-v${SHERPA_VERSION}-osx-x64-static-lib.tar.bz2"
        ORT_ARCHIVE="onnxruntime-osx-x86_64-${ORT_VERSION}.tgz"
        ORT_LIBNAME="libonnxruntime.${ORT_VERSION}.dylib"
        ORT_LINK="libonnxruntime.dylib"
    elif [ "$OS" = "Linux" ] && [ "$ARCH" = "x86_64" ]; then
        STATIC_ARCHIVE="sherpa-onnx-v${SHERPA_VERSION}-linux-x64-static-lib.tar.bz2"
        ORT_ARCHIVE="onnxruntime-linux-x64-${ORT_VERSION}.tgz"
        ORT_LIBNAME="libonnxruntime.so.${ORT_VERSION}"
        ORT_LINK="libonnxruntime.so"
    elif [ "$OS" = "Linux" ] && [ "$ARCH" = "aarch64" ]; then
        STATIC_ARCHIVE="sherpa-onnx-v${SHERPA_VERSION}-linux-aarch64-static-lib.tar.bz2"
        ORT_ARCHIVE="onnxruntime-linux-aarch64-${ORT_VERSION}.tgz"
        ORT_LIBNAME="libonnxruntime.so.${ORT_VERSION}"
        ORT_LINK="libonnxruntime.so"
    else
        echo "ERROR: Unsupported platform: $OS $ARCH"
        exit 1
    fi

    SHERPA_URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/v${SHERPA_VERSION}"
    ORT_URL="https://github.com/microsoft/onnxruntime/releases/download/v${ORT_VERSION}"

    # Download sherpa-onnx static archive (all .a files)
    if [ ! -f "$CACHE/$STATIC_ARCHIVE" ]; then
        echo "==> Downloading sherpa-onnx static libs..."
        curl -fSL "$SHERPA_URL/$STATIC_ARCHIVE" -o "$CACHE/$STATIC_ARCHIVE"
    fi

    # Download ORT shared library from Microsoft
    if [ ! -f "$CACHE/$ORT_ARCHIVE" ]; then
        echo "==> Downloading ONNX Runtime ${ORT_VERSION}..."
        curl -fSL "$ORT_URL/$ORT_ARCHIVE" -o "$CACHE/$ORT_ARCHIVE"
    fi

    # Extract static libs
    echo "==> Extracting sherpa-onnx static libs..."
    STATIC_STEM="${STATIC_ARCHIVE%.tar.bz2}"
    tar -xjf "$CACHE/$STATIC_ARCHIVE" -C "$CACHE"
    cp "$CACHE/$STATIC_STEM/lib/"*.a "$LIB_DIR/"

    # Extract ORT dylib
    echo "==> Extracting libonnxruntime..."
    ORT_STEM="${ORT_ARCHIVE%.tgz}"
    tar -xzf "$CACHE/$ORT_ARCHIVE" -C "$CACHE"
    cp "$CACHE/$ORT_STEM/lib/$ORT_LIBNAME" "$LIB_DIR/"
    ln -sf "$ORT_LIBNAME" "$LIB_DIR/$ORT_LINK"
    # soname symlink (e.g. libonnxruntime.so.1) needed at runtime
    SONAME=$(echo "$ORT_LIBNAME" | sed 's/\.[0-9]*\.[0-9]*$//')
    if [ "$SONAME" != "$ORT_LIBNAME" ] && [ "$SONAME" != "$ORT_LINK" ]; then
        ln -sf "$ORT_LIBNAME" "$LIB_DIR/$SONAME"
    fi

    # Replace libonnxruntime.a with an empty stub archive.
    # sherpa-onnx-sys emits `static=onnxruntime` which expects this file,
    # but we want ORT symbols to come from the shared library only.
    echo "==> Creating stub libonnxruntime.a..."
    rm -f "$LIB_DIR/libonnxruntime.a"
    # macOS ar doesn't support creating empty archives with `ar rcs`.
    # Create a trivial .o with an empty .c file, archive it, then clean up.
    STUB_C=$(mktemp /tmp/ort_stub.XXXXXX.c)
    STUB_O="${STUB_C%.c}.o"
    : > "$STUB_C"
    cc -c "$STUB_C" -o "$STUB_O"
    ar rcs "$LIB_DIR/libonnxruntime.a" "$STUB_O"
    rm -f "$STUB_C" "$STUB_O"

    touch "$MARKER"
    echo "==> Desktop sherpa-onnx ready (shared ORT)"
    ls -lh "$LIB_DIR/libonnxruntime"*

# Export and download neural grammar models
_setup-grammar:
    #!/usr/bin/env bash
    set -euo pipefail
    GRAMMAR_DIR="{{tauri_dir}}/data/grammar"
    VERSION="0.0.1"
    FILES=(
        "cola_model_quantized.${VERSION}.onnx"
        "cola_tokenizer.${VERSION}.json"
        "encoder_model_quantized.${VERSION}.onnx"
        "decoder_with_past_quantized.${VERSION}.onnx"
        "cross_attn_kv_weights.${VERSION}.bin"
        "t5_tokenizer.${VERSION}.json"
        "config.${VERSION}.json"
    )
    complete=true
    for f in "${FILES[@]}"; do [ -f "$GRAMMAR_DIR/$f" ] || complete=false; done
    if $complete; then
        echo "==> Grammar models already present at $GRAMMAR_DIR"
        exit 0
    fi
    echo "==> Preparing grammar models..."
    mkdir -p "$GRAMMAR_DIR"
    VENV_DIR="{{repo_root}}/.grammar-venv"
    [ -d "$VENV_DIR" ] || python3 -m venv "$VENV_DIR"
    "$VENV_DIR/bin/pip" install -q --upgrade pip
    "$VENV_DIR/bin/pip" install -q huggingface_hub transformers "optimum[onnxruntime]" onnx numpy torch
    echo "==> Exporting CoLA router..."
    "$VENV_DIR/bin/python" "{{repo_root}}/scripts/export_cola_onnx.py" --output-dir "$GRAMMAR_DIR" --version "$VERSION"
    echo "==> Downloading T5 corrector..."
    "$VENV_DIR/bin/python" "{{repo_root}}/scripts/download_t5_grammar_onnx.py" --output-dir "$GRAMMAR_DIR" --version "$VERSION"
    echo "==> Grammar models ready — rebuild to embed them"

# First-time Android setup: sherpa-onnx libs, ORT shared library, grammar models
setup-android:
    SHERPA_ONNX_LIB_DIR="{{sherpa_libs}}" {{repo_root}}/scripts/android-build.sh --setup-only

# Build debug APK (default)
apk: _ensure-keystore (_build "debug")

# Build release APK
apk-release: _ensure-keystore (_build "release")

# Run Rust library tests
test:
    cd {{tauri_dir}} && cargo test --lib

# Cargo check (fast compile check)
check:
    cd {{tauri_dir}} && cargo check

# Run pipeline eval harness (pass extra args after --)
eval *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    ORT_LIB_DIR="{{desktop_deps}}/sherpa-onnx/lib"
    # Find the versioned ORT shared library for ort crate's load-dynamic
    ORT_DYLIB=$(find "$ORT_LIB_DIR" -name 'libonnxruntime.so.*.*' -o -name 'libonnxruntime.*.dylib' 2>/dev/null | head -1)
    if [ -z "${ORT_DYLIB:-}" ]; then
        echo "ERROR: ORT shared library not found in $ORT_LIB_DIR"
        echo "Run: just setup"
        exit 1
    fi
    cd {{tauri_dir}}
    SHERPA_ONNX_LIB_DIR="$ORT_LIB_DIR" \
        ORT_DYLIB_PATH="$ORT_DYLIB" \
        LD_LIBRARY_PATH="$ORT_LIB_DIR:${LD_LIBRARY_PATH:-}" \
        DYLD_LIBRARY_PATH="$ORT_LIB_DIR:${DYLD_LIBRARY_PATH:-}" \
        cargo run --bin eval_pipeline -- {{ARGS}}

# Clean all build artifacts
clean:
    cd {{tauri_dir}} && cargo clean
    cd {{android_dir}} && ./gradlew clean

# ── Internal recipes ──

_build profile: _tauri-build _strip _repackage _sign
    @echo ""
    @echo "APK ready: {{repo_root}}/verba.apk"
    @ls -lh {{repo_root}}/verba.apk

# Build via Tauri CLI (handles Rust compilation, frontend bundling, and Gradle packaging)
_tauri-build:
    @echo "==> Building with Tauri CLI (arm64)..."
    @test -f {{sherpa_libs}}/libsherpa-onnx-c-api.a || (echo "ERROR: sherpa-onnx libs not found at {{sherpa_libs}}" && echo "Run: just setup-android" && exit 1)
    # NB: the Rust link API level (and gradle minSdk) come from
    # bundle.android.minSdkVersion in tauri.conf.json — 26, required because
    # cpal 0.18's Android host is AAudio-only (libaaudio.so ships from API 26).
    cd {{repo_root}} && SHERPA_ONNX_LIB_DIR="{{sherpa_libs}}" npx tauri android build --target aarch64 --apk

# Strip debug symbols from .so to reduce APK size
_strip:
    @echo "==> Stripping native library..."
    {{strip_bin}} --strip-unneeded {{jni_dir}}/arm64-v8a/libverba_rs_lib.so
    rm -rf {{jni_dir}}/x86_64
    @ls -lh {{jni_dir}}/arm64-v8a/libverba_rs_lib.so

# Re-run Gradle to package the stripped .so into the APK
_repackage:
    @echo "==> Repackaging APK with stripped library..."
    cd {{android_dir}} && ./gradlew assembleUniversalRelease \
        -x rustBuildArm64Release \
        -x rustBuildArmRelease \
        -x rustBuildX86_64Release \
        -x rustBuildX86Release

# Align and sign the APK
_sign:
    @echo "==> Signing APK..."
    {{build_tools}}/zipalign -f 4 \
        {{android_dir}}/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk \
        /tmp/verba-aligned.apk
    {{build_tools}}/apksigner sign \
        --ks {{keystore}} \
        --ks-pass pass:android \
        --key-pass pass:android \
        --out {{repo_root}}/verba.apk \
        /tmp/verba-aligned.apk
    rm -f /tmp/verba-aligned.apk

# Generate debug keystore if it doesn't exist
_ensure-keystore:
    @test -f {{keystore}} || ( \
        echo "==> Generating debug keystore..." && \
        keytool -genkey -v \
            -keystore {{keystore}} \
            -alias debug \
            -keyalg RSA -keysize 2048 \
            -validity 10000 \
            -storepass android \
            -keypass android \
            -dname "CN=Debug" \
    )
