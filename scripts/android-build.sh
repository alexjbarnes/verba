#!/usr/bin/env bash
#
# Build Scribe for Android (arm64-v8a).
#
# NOTE: For day-to-day builds, use `just apk` instead. This script is
# primarily useful for first-time setup (--setup-only) to build and cache
# the sherpa-onnx native libraries.
#
# Usage:
#   ./scripts/android-build.sh --setup-only # build sherpa-onnx libs (first time)
#   ./scripts/android-build.sh              # debug APK (prefer `just apk`)
#   ./scripts/android-build.sh --release    # release APK (prefer `just apk-release`)
#
# Prerequisites (installed automatically where possible):
#   - Android SDK with platform 34 (ANDROID_HOME)
#   - Android NDK r28+ (ANDROID_NDK_HOME)
#   - JDK 17+
#   - Rust aarch64-linux-android target
#   - cmake, ninja-build
#
# The script builds sherpa-onnx native libraries from source on first run
# and caches them for subsequent builds.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
TAURI_DIR="$REPO_ROOT/src-tauri"
ANDROID_PROJECT="$TAURI_DIR/gen/android"
SHERPA_ONNX_VERSION="1.12.34"
BUILD_TYPE="debug"
SETUP_ONLY=false

# ── Parse args ──

for arg in "$@"; do
    case "$arg" in
        --release) BUILD_TYPE="release" ;;
        --setup-only) SETUP_ONLY=true ;;
        --help|-h)
            head -17 "$0" | tail -14
            exit 0
            ;;
    esac
done

# ── Helpers ──

info()  { echo "==> $*"; }
warn()  { echo "WARNING: $*" >&2; }
die()   { echo "ERROR: $*" >&2; exit 1; }

check_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "$1 not found. $2"
}

# ── Step 1: Check prerequisites ──

info "Checking prerequisites..."

check_cmd rustup "Install from https://rustup.rs"
check_cmd cargo  "Install from https://rustup.rs"
check_cmd cmake  "Install with: apt install cmake / brew install cmake"
check_cmd java   "Install JDK 17+: apt install openjdk-17-jdk / brew install openjdk@17"

# Rust Android target
if ! rustup target list --installed | grep -q aarch64-linux-android; then
    info "Installing Rust target aarch64-linux-android..."
    rustup target add aarch64-linux-android
fi

# Android SDK
if [ -z "${ANDROID_HOME:-}" ]; then
    # Common locations
    for candidate in \
        "$HOME/Android/Sdk" \
        "$HOME/Library/Android/sdk" \
        "/opt/android-sdk" \
        "$HOME/.android/sdk"; do
        if [ -d "$candidate" ]; then
            export ANDROID_HOME="$candidate"
            break
        fi
    done
fi
[ -d "${ANDROID_HOME:-}" ] || die "ANDROID_HOME not set and Android SDK not found.
  Install Android Studio or set ANDROID_HOME manually.
  See: https://developer.android.com/studio"

info "Android SDK: $ANDROID_HOME"

# Android NDK
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
    # Find the latest installed NDK
    NDK_DIR="$ANDROID_HOME/ndk"
    if [ -d "$NDK_DIR" ]; then
        LATEST_NDK=$(ls -1 "$NDK_DIR" 2>/dev/null | sort -V | tail -1)
        if [ -n "$LATEST_NDK" ]; then
            export ANDROID_NDK_HOME="$NDK_DIR/$LATEST_NDK"
        fi
    fi
fi
[ -d "${ANDROID_NDK_HOME:-}" ] || die "ANDROID_NDK_HOME not set and no NDK found.
  Install via: sdkmanager --install 'ndk;28.0.13004108'
  Or set ANDROID_NDK_HOME manually."

NDK_VERSION=$(basename "$ANDROID_NDK_HOME" | cut -d. -f1)
if [ "$NDK_VERSION" -lt 28 ] 2>/dev/null; then
    warn "NDK version $NDK_VERSION < 28. Google Play requires NDK 28+ for 16KB page alignment."
fi
info "Android NDK: $ANDROID_NDK_HOME"

# Tauri CLI
if ! npx tauri --version >/dev/null 2>&1; then
    die "Tauri CLI not found. Run: npm install @tauri-apps/cli"
fi

# ── Step 2: Download ORT Android shared library + headers ──
#
# sherpa-onnx cmake links against the pre-built libonnxruntime.so rather than
# building ORT from source. ORT must be downloaded BEFORE sherpa-onnx so the
# cmake invocation can find the headers and .so via SHERPA_ONNXRUNTIME_INCLUDE_DIR
# and SHERPA_ONNXRUNTIME_LIB_DIR env vars (read by sherpa-onnx/cmake/onnxruntime.cmake).

ORT_VERSION="1.24.2"
ORT_CACHE="$REPO_ROOT/.android-deps/ort"
ORT_LIB_DIR="$ORT_CACHE/arm64-v8a"
ORT_LIB="$ORT_LIB_DIR/libonnxruntime.so"
ORT_HEADERS_DIR="$ORT_CACHE/headers"

if [ -f "$ORT_LIB" ] && [ -f "$ORT_HEADERS_DIR/onnxruntime_cxx_api.h" ]; then
    info "Using cached ORT library and headers at $ORT_CACHE"
else
    info "Downloading ORT v${ORT_VERSION} Android library and headers..."
    mkdir -p "$ORT_LIB_DIR" "$ORT_HEADERS_DIR"

    ORT_AAR_URL="https://repo1.maven.org/maven2/com/microsoft/onnxruntime/onnxruntime-android/${ORT_VERSION}/onnxruntime-android-${ORT_VERSION}.aar"
    ORT_AAR="$ORT_CACHE/onnxruntime-android.aar"

    check_cmd curl "Install curl: apt install curl"
    check_cmd unzip "Install unzip: apt install unzip"

    curl -fsSL -o "$ORT_AAR" "$ORT_AAR_URL"
    # Extract the arm64-v8a shared library
    unzip -p "$ORT_AAR" "jni/arm64-v8a/libonnxruntime.so" > "$ORT_LIB"
    # Extract headers so sherpa-onnx can compile against shared ORT
    unzip -o "$ORT_AAR" "headers/*" -d "$ORT_CACHE"
    rm "$ORT_AAR"

    [ -s "$ORT_LIB" ] || die "Failed to extract libonnxruntime.so from ORT AAR"
    [ -f "$ORT_HEADERS_DIR/onnxruntime_cxx_api.h" ] || \
        die "ORT headers not found at $ORT_HEADERS_DIR — check that 'headers/onnxruntime_cxx_api.h' exists in the AAR"
    info "ORT cached at $ORT_CACHE ($(du -sh "$ORT_LIB" | cut -f1))"
fi

export ORT_LIB_DIR
export SHERPA_ONNXRUNTIME_INCLUDE_DIR="$ORT_HEADERS_DIR"
export SHERPA_ONNXRUNTIME_LIB_DIR="$ORT_LIB_DIR"

# ── Step 3: Build sherpa-onnx native libraries ──
#
# sherpa-onnx/cmake/onnxruntime.cmake reads SHERPA_ONNXRUNTIME_INCLUDE_DIR and
# SHERPA_ONNXRUNTIME_LIB_DIR (set above). It creates an IMPORTED SHARED target
# for onnxruntime and installs libonnxruntime.so into install/lib/. No static ORT
# is embedded — libsherpa-onnx-c-api.a has ORT symbols as external references
# resolved at load time from libonnxruntime.so.
#
# The marker file .shared-ort distinguishes this shared-ORT build from the legacy
# static-ORT build. If the cache exists without the marker, it is rebuilt.

SHERPA_ONNX_CACHE="$REPO_ROOT/.android-deps/sherpa-onnx"
SHERPA_ONNX_LIB_DIR="$SHERPA_ONNX_CACHE/install/lib"
SHERPA_SHARED_ORT_MARKER="$SHERPA_ONNX_LIB_DIR/.shared-ort"

if [ -d "$SHERPA_ONNX_LIB_DIR" ] && \
   [ -f "$SHERPA_ONNX_LIB_DIR/libsherpa-onnx-c-api.a" ] && \
   [ -f "$SHERPA_SHARED_ORT_MARKER" ]; then
    info "Using cached sherpa-onnx libraries (shared ORT) from $SHERPA_ONNX_LIB_DIR"
else
    if [ -d "$SHERPA_ONNX_LIB_DIR" ] && \
       [ -f "$SHERPA_ONNX_LIB_DIR/libsherpa-onnx-c-api.a" ] && \
       [ ! -f "$SHERPA_SHARED_ORT_MARKER" ]; then
        info "Sherpa-onnx cache was built with static ORT — rebuilding with shared ORT..."
        rm -rf "$SHERPA_ONNX_CACHE/install"
    fi

    info "Building sherpa-onnx v${SHERPA_ONNX_VERSION} for Android arm64-v8a (this takes 10-15 minutes)..."

    SHERPA_SRC="$SHERPA_ONNX_CACHE/src"
    SHERPA_BUILD="$SHERPA_ONNX_CACHE/build"

    mkdir -p "$SHERPA_ONNX_CACHE"

    # Clone or update source
    if [ -d "$SHERPA_SRC/.git" ]; then
        info "Updating sherpa-onnx source..."
        git -C "$SHERPA_SRC" fetch --depth 1 origin "v${SHERPA_ONNX_VERSION}"
        git -C "$SHERPA_SRC" checkout FETCH_HEAD
    else
        info "Cloning sherpa-onnx v${SHERPA_ONNX_VERSION}..."
        git clone --depth 1 --branch "v${SHERPA_ONNX_VERSION}" \
            https://github.com/k2-fsa/sherpa-onnx.git "$SHERPA_SRC"
    fi

    # CMake cross-compile. SHERPA_ONNXRUNTIME_INCLUDE_DIR / SHERPA_ONNXRUNTIME_LIB_DIR
    # are exported above so sherpa-onnx cmake finds the pre-built ORT .so and headers.
    TOOLCHAIN="$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake"
    [ -f "$TOOLCHAIN" ] || die "NDK toolchain not found at $TOOLCHAIN"

    rm -rf "$SHERPA_BUILD"
    mkdir -p "$SHERPA_BUILD"

    cmake -S "$SHERPA_SRC" -B "$SHERPA_BUILD" \
        -DCMAKE_TOOLCHAIN_FILE="$TOOLCHAIN" \
        -DANDROID_ABI=arm64-v8a \
        -DANDROID_PLATFORM=android-28 \
        -DCMAKE_BUILD_TYPE=Release \
        -DBUILD_SHARED_LIBS=OFF \
        -DSHERPA_ONNX_ENABLE_C_API=ON \
        -DSHERPA_ONNX_ENABLE_BINARY=OFF \
        -DSHERPA_ONNX_ENABLE_TTS=OFF \
        -DSHERPA_ONNX_ENABLE_JNI=OFF \
        -DSHERPA_ONNX_ENABLE_PYTHON=OFF \
        -DSHERPA_ONNX_ENABLE_TESTS=OFF \
        -DSHERPA_ONNX_ENABLE_CHECK=OFF \
        -DSHERPA_ONNX_ENABLE_PORTAUDIO=OFF \
        -DSHERPA_ONNX_ENABLE_WEBSOCKET=OFF \
        -DCMAKE_INSTALL_PREFIX="$SHERPA_ONNX_CACHE/install"

    cmake --build "$SHERPA_BUILD" --config Release -j "$(nproc 2>/dev/null || echo 4)"
    cmake --install "$SHERPA_BUILD"

    if [ ! -f "$SHERPA_ONNX_LIB_DIR/libsherpa-onnx-c-api.a" ]; then
        # Some builds put libs in lib64 or other locations
        for candidate in \
            "$SHERPA_ONNX_CACHE/install/lib64" \
            "$SHERPA_BUILD/lib" \
            "$SHERPA_BUILD/lib64"; do
            if [ -f "$candidate/libsherpa-onnx-c-api.a" ]; then
                mkdir -p "$SHERPA_ONNX_LIB_DIR"
                cp "$candidate"/*.a "$SHERPA_ONNX_LIB_DIR/"
                break
            fi
        done
    fi

    [ -f "$SHERPA_ONNX_LIB_DIR/libsherpa-onnx-c-api.a" ] || \
        die "sherpa-onnx build succeeded but libsherpa-onnx-c-api.a not found in expected locations."

    # Verify cmake install copied libonnxruntime.so (from SHERPA_ONNXRUNTIME_LIB_DIR)
    [ -f "$SHERPA_ONNX_LIB_DIR/libonnxruntime.so" ] || \
        die "libonnxruntime.so not found in $SHERPA_ONNX_LIB_DIR — cmake install may have failed"

    # Create an empty stub libonnxruntime.a alongside libonnxruntime.so.
    # sherpa-onnx-sys emits `static=onnxruntime` which expects a .a in SHERPA_ONNX_LIB_DIR.
    # The stub satisfies that linker directive without embedding ORT code; the real ORT
    # symbols are resolved from libonnxruntime.so via `dylib=onnxruntime` in build.rs.
    NDK_CLANG="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android28-clang"
    NDK_AR="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar"
    echo 'typedef int _ort_stub_t;' > /tmp/_ort_stub.c
    "$NDK_CLANG" -c /tmp/_ort_stub.c -o /tmp/_ort_stub.o
    "$NDK_AR" crs "$SHERPA_ONNX_LIB_DIR/libonnxruntime.a" /tmp/_ort_stub.o
    rm /tmp/_ort_stub.c /tmp/_ort_stub.o
    touch "$SHERPA_SHARED_ORT_MARKER"

    info "sherpa-onnx libraries built and cached at $SHERPA_ONNX_LIB_DIR"

    # Clean build dir to save space (keep source for rebuilds)
    rm -rf "$SHERPA_BUILD"
fi

export SHERPA_ONNX_LIB_DIR

# ── Step 4: Prepare grammar models ──
#
# Generate and embed the CoLA router + T5 corrector ONNX files.
# Placed in src-tauri/data/grammar/ so build.rs picks them up via include_bytes!.

GRAMMAR_DIR="$TAURI_DIR/data/grammar"
GRAMMAR_FILES=(
    cola_model_quantized.onnx
    cola_tokenizer.json
    encoder_model_quantized.onnx
    decoder_model_quantized.onnx
    t5_tokenizer.json
)

grammar_complete() {
    for f in "${GRAMMAR_FILES[@]}"; do
        [ -f "$GRAMMAR_DIR/$f" ] || return 1
    done
    return 0
}

if grammar_complete; then
    info "Using cached grammar models from $GRAMMAR_DIR"
else
    info "Preparing grammar models..."
    mkdir -p "$GRAMMAR_DIR"

    if ! command -v python3 >/dev/null 2>&1; then
        warn "python3 not found — grammar neural correction will not be bundled (nlprule fallback active)"
    else
        # Install Python deps quietly into a local venv to avoid polluting system Python.
        VENV_DIR="$REPO_ROOT/.android-deps/grammar-venv"
        if [ ! -d "$VENV_DIR" ]; then
            python3 -m venv "$VENV_DIR"
        fi
        PIP="$VENV_DIR/bin/pip"
        PYTHON="$VENV_DIR/bin/python"

        info "Installing grammar model Python deps..."
        "$PIP" install -q --upgrade pip
        # CPU-only torch — GPU build pulls in 2-3GB of NVIDIA CUDA libraries we don't need.
        "$PIP" install -q huggingface_hub transformers "optimum[onnxruntime]"

        info "Exporting CoLA router (pszemraj/electra-small-discriminator-CoLA)..."
        "$PYTHON" "$SCRIPT_DIR/export_cola_onnx.py" --output-dir "$GRAMMAR_DIR"

        info "Downloading T5 corrector (visheratin/t5-efficient-tiny-grammar-correction)..."
        "$PYTHON" "$SCRIPT_DIR/download_t5_grammar_onnx.py" --output-dir "$GRAMMAR_DIR"

        if grammar_complete; then
            info "Grammar models ready at $GRAMMAR_DIR"
        else
            warn "Grammar model setup incomplete — nlprule fallback will be active"
        fi
    fi
fi

# ── Step 5: Initialize Tauri Android project ──

if [ ! -d "$ANDROID_PROJECT" ]; then
    info "Initializing Tauri Android project..."
    cd "$REPO_ROOT"
    npx tauri android init
    info "Android project created at $ANDROID_PROJECT"
else
    info "Tauri Android project already exists"
fi

# ── Step 6: Configure Android permissions ──

MANIFEST="$ANDROID_PROJECT/app/src/main/AndroidManifest.xml"
if [ -f "$MANIFEST" ]; then
    # Add RECORD_AUDIO permission if not present
    if ! grep -q "RECORD_AUDIO" "$MANIFEST"; then
        info "Adding RECORD_AUDIO permission to AndroidManifest.xml..."
        sed -i 's|<application|<uses-permission android:name="android.permission.RECORD_AUDIO" />\n    <application|' "$MANIFEST"
    fi
    # Add INTERNET permission if not present (for model downloads)
    if ! grep -q "android.permission.INTERNET" "$MANIFEST"; then
        info "Adding INTERNET permission to AndroidManifest.xml..."
        sed -i 's|<application|<uses-permission android:name="android.permission.INTERNET" />\n    <application|' "$MANIFEST"
    fi
fi

# ── Step 7: Build ──

if $SETUP_ONLY; then
    info "Setup complete. Run without --setup-only to build."
    exit 0
fi

info "Building Scribe for Android ($BUILD_TYPE)..."
cd "$REPO_ROOT"

BUILD_ARGS=()
if [ "$BUILD_TYPE" = "release" ]; then
    BUILD_ARGS+=(--release)
fi

# Pass the library path and NDK linker setup through cargo config
export SHERPA_ONNX_LIB_DIR
export ANDROID_HOME
export ANDROID_NDK_HOME

npx tauri android build "${BUILD_ARGS[@]}"

# ── Done ──

if [ "$BUILD_TYPE" = "release" ]; then
    APK_DIR="$ANDROID_PROJECT/app/build/outputs/apk/universal/release"
else
    APK_DIR="$ANDROID_PROJECT/app/build/outputs/apk/universal/debug"
fi

if [ -d "$APK_DIR" ]; then
    info "APK built:"
    ls -lh "$APK_DIR"/*.apk 2>/dev/null || true
else
    info "Build complete. Check $ANDROID_PROJECT/app/build/outputs/ for APK."
fi
