---
category: context
subject: android-build-environment
---

Android APK builds work again as of 2026-10-10 after the sandbox lost the whole toolchain (empty ~/Android/Sdk, no JDK, no aarch64-linux-android rustup target; verba.apk was also left as a truncated 3.3MB zip). Reinstalled with: apt openjdk-17-jdk-headless; cmdline-tools (zips land at ~/Android/Sdk/cmdline-tools/latest, `mv cmdline-tools latest`); sdkmanager --install platform-tools, platforms;android-36, build-tools;35.0.0, ndk;28.0.13004108 (all the versions the project uses); rustup target add aarch64-linux-android. Then `just apk` works unchanged (justfile defaults ANDROID_HOME=~/Android/Sdk, JAVA_HOME=/usr). First build after a wipe re-downloads the whole cargo registry and Gradle deps, so it takes ~15+ min and a background task can be killed by a session restart mid-build (signal 15) without leaving a marker; check verba.apk size/timestamp and the task output tail before assuming failure, and just rerun.
