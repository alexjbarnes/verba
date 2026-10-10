# verba

A local reading app: save texts, import URLs and files, follow RSS feeds, and
read them in a clean reader. Desktop (Tauri v2, Rust + vanilla HTML/JS) and
Android.

This is the continuation of the original verba (offline dictation + reading).
The speech-recognition and local-TTS halves were removed; the archive lives at
`../verba-archive`. Server-side TTS may return later.

## Build

```bash
just dev            # desktop dev loop
just apk            # signed debug APK (same key as the old builds — installs as an update)
just apk-release    # release APK
just test           # Rust library tests
```

No native ML dependencies remain; a plain `npx tauri android build` works
without any setup step beyond the Android SDK/NDK.
