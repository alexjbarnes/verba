# Verba build automation (reading app)
#
# Usage:
#   just dev           # desktop dev loop (hot-reload)
#   just apk           # build debug APK (signed with debug.keystore)
#   just apk-release   # build release APK
#   just test          # run Rust tests
#   just check         # cargo check
#   just clean         # clean all build artifacts

repo_root    := justfile_directory()
tauri_dir    := repo_root / "src-tauri"
android_dir  := tauri_dir / "gen" / "android"
jni_dir      := android_dir / "app" / "src" / "main" / "jniLibs"
keystore     := repo_root / "debug.keystore"

# Auto-detect paths
android_home := env("ANDROID_HOME", `echo ${HOME}/Android/Sdk`)
android_ndk  := env("ANDROID_NDK_HOME", `ls -1d ${ANDROID_HOME:-$HOME/Android/Sdk}/ndk/* 2>/dev/null | sort -V | tail -1 || echo ""`)
build_tools  := `ls -1d ${ANDROID_HOME:-$HOME/Android/Sdk}/build-tools/* 2>/dev/null | sort -V | tail -1 || echo ""`
strip_bin    := android_ndk / "toolchains" / "llvm" / "prebuilt" / "linux-x86_64" / "bin" / "llvm-strip"

export ANDROID_HOME := android_home
export ANDROID_NDK_HOME := android_ndk
export JAVA_HOME := env("JAVA_HOME", "/usr")

# ── Recipes ──

# Fast desktop dev loop: hot-reload.
dev:
    npx tauri dev

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
    cd {{repo_root}} && npx tauri android build --target aarch64 --apk

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

# Generate debug keystore if it doesn't exist.
# NOTE: the real keystore is copied from the old repo and must persist — a
# freshly generated one cannot update already-installed APKs.
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
