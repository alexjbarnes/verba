package com.alexb151.verba

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.support.v4.media.MediaMetadataCompat
import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat
import android.widget.Toast
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

class VerbaApp : Application() {
    companion object {
        var instance: VerbaApp? = null
        var nativeLoadError: String? = null

        private const val CHANNEL_ID = "verba_tts"
        // internal: MediaActionReceiver cancels a session-less (stale) notification.
        internal const val NOTIFICATION_ID = 42

        @JvmStatic
        fun showToast(msg: String) {
            val app = instance ?: return
            Handler(Looper.getMainLooper()).post {
                Toast.makeText(app, msg, Toast.LENGTH_SHORT).show()
            }
        }

        @JvmStatic
        fun copyToClipboard(text: String) {
            val app = instance ?: return
            // Clipboard writes must run on a Looper thread; the JNI call arrives
            // on a Rust-attached thread, so hop to the main looper.
            Handler(Looper.getMainLooper()).post {
                val cm = app.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
                    ?: return@post
                cm.setPrimaryClip(ClipData.newPlainText("Verba", text))
            }
        }

        // ── Selection-toolbar report menu ──
        //
        // Whether SelectionReportLayout should add "Report mispronunciation" to
        // the text-selection floating toolbar. Off by default; the frontend turns
        // it on only while the reading panel (Listen mode) is showing. @JvmStatic
        // on the property itself (rather than a separate setter function) is what
        // gives a real static setReportMenuEnabled(Z)V for lib.rs's JNI call --
        // a separate function of that name clashes with the property's own
        // generated accessor (same JVM signature).
        @Volatile
        @JvmStatic
        var reportMenuEnabled: Boolean = false

        private var audioFocusRequest: AudioFocusRequest? = null

        @JvmStatic
        fun requestAudioFocus(): Boolean {
            val app = instance ?: return false
            val am = app.getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return false
            val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_ASSISTANCE_ACCESSIBILITY)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build()
                )
                .build()
            val result = am.requestAudioFocus(req)
            return if (result == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
                audioFocusRequest = req
                true
            } else {
                false
            }
        }

        @JvmStatic
        fun abandonAudioFocus(): Boolean {
            val app = instance ?: return false
            val am = app.getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return false
            val req = audioFocusRequest ?: return false
            audioFocusRequest = null
            return am.abandonAudioFocusRequest(req) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        }

        // ── TTS MediaSession ──

        private var mediaSession: MediaSessionCompat? = null
        private var ttsAudioFocus: AudioFocusRequest? = null
        // internal: MediaActionReceiver reads the position for its ±15s seeks.
        internal var lastPositionMs: Long = 0
        private var lastDurationMs: Long = 0
        private var sessionActive = false

        // Real playback-paused state, as last reported by updateMediaSession.
        // internal: MediaActionReceiver reads it to toggle play/pause.
        @Volatile internal var lastPaused: Boolean = false

        // Whether a live session exists for the notification's buttons to drive
        // (MediaActionReceiver's guard against taps on a stale notification).
        internal fun hasMediaSession(): Boolean = mediaSession != null

        // Whether we auto-paused for an AUDIOFOCUS_LOSS_TRANSIENT (e.g. a phone
        // call) and should resume when focus comes back. See requestTtsAudioFocus
        // and updateMediaSession.
        @Volatile private var wasPlayingAtLoss: Boolean = false

        // One-shot marker for the pause that requestTtsAudioFocus's own
        // nativeTtsPause() call is about to produce, so updateMediaSession doesn't
        // mistake that echo for a user-initiated pause and clear wasPlayingAtLoss
        // out from under it. Any OTHER transition into paused — including the
        // user explicitly pausing mid-call — reaches updateMediaSession with this
        // already consumed, and correctly clears wasPlayingAtLoss so playback
        // doesn't resume itself when focus returns.
        @Volatile private var expectingFocusLossPause: Boolean = false

        // Real title for the notification + lock-screen metadata, set by the
        // frontend via setMediaTitle. Falls back to "Verba Reader" when blank.
        @Volatile private var mediaTitle: String = ""

        @JvmStatic
        fun startMediaSession() {
            val app = instance ?: return
            // Idempotent: a mid-listen re-render (speed/voice change, seek) calls
            // this again, but re-requesting audio focus while it's already held
            // fires the existing listener's AUDIOFOCUS_LOSS -> nativeTtsPause(),
            // which paused playback once buffering finished. Keep the one session.
            if (sessionActive) return
            sessionActive = true
            lastPositionMs = 0
            lastDurationMs = 0
            lastPaused = false
            wasPlayingAtLoss = false
            expectingFocusLossPause = false
            Handler(Looper.getMainLooper()).post {
                requestTtsAudioFocus(app)
                createNotificationChannel(app)

                val session = MediaSessionCompat(app, "VerbaTTS")
                session.setCallback(object : MediaSessionCompat.Callback() {
                    override fun onPlay() { nativeTtsResume() }
                    override fun onPause() { nativeTtsPause() }
                    override fun onStop() { nativeTtsStop() }
                    override fun onSeekTo(pos: Long) { nativeTtsSeek(pos) }
                    override fun onSkipToNext() { nativeTtsSeek(lastPositionMs + 15000) }
                    override fun onSkipToPrevious() { nativeTtsSeek(maxOf(0, lastPositionMs - 15000)) }
                    override fun onRewind() { nativeTtsSeek(maxOf(0, lastPositionMs - 15000)) }
                    override fun onFastForward() { nativeTtsSeek(lastPositionMs + 15000) }
                })
                session.isActive = true
                mediaSession = session
                showNotification(app, false)
            }
        }

        @JvmStatic
        fun updateMediaSession(positionMs: Long, durationMs: Long, paused: Boolean) {
            lastPositionMs = positionMs
            lastDurationMs = durationMs
            // Rising edge into paused. A transition we didn't just trigger
            // ourselves via requestTtsAudioFocus (expectingFocusLossPause) is a
            // real user pause -- possibly one taken mid-call -- so it must cancel
            // any pending auto-resume. Repeated paused=true reports (no edge,
            // same state as last time) are ignored so they can't clear it moments
            // later.
            if (paused && !lastPaused) {
                if (expectingFocusLossPause) {
                    expectingFocusLossPause = false
                } else {
                    wasPlayingAtLoss = false
                }
            }
            lastPaused = paused

            val session = mediaSession ?: return
            val state = PlaybackStateCompat.Builder()
                .setActions(
                    PlaybackStateCompat.ACTION_PLAY or
                    PlaybackStateCompat.ACTION_PAUSE or
                    PlaybackStateCompat.ACTION_PLAY_PAUSE or
                    PlaybackStateCompat.ACTION_STOP or
                    PlaybackStateCompat.ACTION_SEEK_TO or
                    PlaybackStateCompat.ACTION_REWIND or
                    PlaybackStateCompat.ACTION_FAST_FORWARD
                )
                .setState(
                    if (paused) PlaybackStateCompat.STATE_PAUSED else PlaybackStateCompat.STATE_PLAYING,
                    positionMs,
                    1f
                )
                .build()
            session.setPlaybackState(state)

            val metadata = MediaMetadataCompat.Builder()
                .putString(MediaMetadataCompat.METADATA_KEY_TITLE, displayTitle())
                .putLong(MediaMetadataCompat.METADATA_KEY_DURATION, durationMs)
                .build()
            session.setMetadata(metadata)

            val app = instance ?: return
            showNotification(app, paused)
        }

        @JvmStatic
        fun setMediaTitle(title: String) {
            mediaTitle = title
        }

        private fun displayTitle(): String = mediaTitle.ifBlank { "Verba Reader" }

        private fun formatClock(ms: Long): String {
            val totalSeconds = (ms / 1000).coerceAtLeast(0)
            val minutes = totalSeconds / 60
            val seconds = totalSeconds % 60
            return "%d:%02d".format(minutes, seconds)
        }

        @JvmStatic
        fun stopMediaSession() {
            val app = instance
            sessionActive = false
            mediaSession?.isActive = false
            mediaSession?.release()
            mediaSession = null
            abandonTtsAudioFocus()
            if (app != null) {
                NotificationManagerCompat.from(app).cancel(NOTIFICATION_ID)
            }
        }

        private fun requestTtsAudioFocus(ctx: Context) {
            val am = ctx.getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
            val focusListener = AudioManager.OnAudioFocusChangeListener { change ->
                when (change) {
                    AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> {
                        // e.g. a phone call. Only arm auto-resume if we were
                        // actually playing (not already paused by the user) --
                        // and only then do we expect the pause below to echo
                        // back through updateMediaSession.
                        wasPlayingAtLoss = !lastPaused
                        expectingFocusLossPause = wasPlayingAtLoss
                        nativeTtsPause()
                    }
                    AudioManager.AUDIOFOCUS_LOSS -> {
                        // Permanent loss: never auto-resume.
                        wasPlayingAtLoss = false
                        expectingFocusLossPause = false
                        nativeTtsPause()
                    }
                    AudioManager.AUDIOFOCUS_GAIN -> {
                        if (wasPlayingAtLoss) nativeTtsResume()
                        wasPlayingAtLoss = false
                        expectingFocusLossPause = false
                    }
                }
            }
            val req = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build()
                )
                .setOnAudioFocusChangeListener(focusListener)
                .build()
            am.requestAudioFocus(req)
            ttsAudioFocus = req
        }

        private fun abandonTtsAudioFocus() {
            val app = instance ?: return
            val am = app.getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
            ttsAudioFocus?.let { am.abandonAudioFocusRequest(it) }
            ttsAudioFocus = null
        }

        private fun createNotificationChannel(ctx: Context) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                val channel = NotificationChannel(
                    CHANNEL_ID, "Reader", NotificationManager.IMPORTANCE_LOW
                )
                channel.setShowBadge(false)
                val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
                nm.createNotificationChannel(channel)
            }
        }

        private fun showNotification(ctx: Context, paused: Boolean) {
            val session = mediaSession ?: return
            val openIntent = ctx.packageManager.getLaunchIntentForPackage(ctx.packageName)
            val contentIntent = if (openIntent != null) {
                PendingIntent.getActivity(ctx, 0, openIntent, PendingIntent.FLAG_IMMUTABLE)
            } else null

            // Explicit broadcasts to MediaActionReceiver, NOT MediaButtonReceiver
            // pending intents: with no MEDIA_BUTTON receiver/service in the
            // manifest, buildMediaButtonPendingIntent returns null and the buttons
            // render but do nothing. Distinct request codes keep the four
            // PendingIntents from collapsing into one.
            fun mediaAction(action: String, requestCode: Int): PendingIntent =
                PendingIntent.getBroadcast(
                    ctx, requestCode,
                    Intent(ctx, MediaActionReceiver::class.java).setAction(action),
                    PendingIntent.FLAG_IMMUTABLE
                )

            val rewindAction = NotificationCompat.Action.Builder(
                android.R.drawable.ic_media_rew, "Rewind 15s",
                mediaAction(MediaActionReceiver.ACTION_REWIND, 1)
            ).build()

            val playPauseAction = if (paused) {
                NotificationCompat.Action.Builder(
                    android.R.drawable.ic_media_play, "Play",
                    mediaAction(MediaActionReceiver.ACTION_PLAY_PAUSE, 2)
                ).build()
            } else {
                NotificationCompat.Action.Builder(
                    android.R.drawable.ic_media_pause, "Pause",
                    mediaAction(MediaActionReceiver.ACTION_PLAY_PAUSE, 2)
                ).build()
            }

            val fastForwardAction = NotificationCompat.Action.Builder(
                android.R.drawable.ic_media_ff, "Fast-forward 15s",
                mediaAction(MediaActionReceiver.ACTION_FAST_FORWARD, 3)
            ).build()

            val exitAction = NotificationCompat.Action.Builder(
                android.R.drawable.ic_menu_close_clear_cancel, "Exit",
                mediaAction(MediaActionReceiver.ACTION_EXIT, 4)
            ).build()

            val notification = NotificationCompat.Builder(ctx, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.ic_lock_silent_mode_off)
                .setContentTitle(displayTitle())
                .setContentText("${formatClock(lastPositionMs)} / ${formatClock(lastDurationMs)}")
                .setContentIntent(contentIntent)
                .addAction(rewindAction)
                .addAction(playPauseAction)
                .addAction(fastForwardAction)
                .addAction(exitAction)
                .setStyle(
                    androidx.media.app.NotificationCompat.MediaStyle()
                        .setMediaSession(session.sessionToken)
                        .setShowActionsInCompactView(0, 1, 2)
                )
                // Stay ongoing (non-swipeable) for the life of the session, paused
                // or not. A swipe-dismiss while merely paused used to cancel the
                // notification but not the session, so the user lost visible
                // controls without ever choosing Exit. Only Exit (ACTION_STOP)
                // tears the session down now.
                .setOngoing(true)
                .setSilent(true)
                .build()

            try {
                NotificationManagerCompat.from(ctx).notify(NOTIFICATION_ID, notification)
            } catch (_: SecurityException) { }
        }

        // ── Hidden-WebView fetch ──
        //
        // Fallback for sites whose HTTP endpoints sit behind a Cloudflare-style
        // JavaScript challenge: every non-browser TLS fingerprint gets a 403, so
        // Rust's reqwest cannot fetch them no matter the headers. An offscreen
        // WebView is a real browser engine — it runs the challenge, earns the
        // clearance cookie, then we re-fetch the URL from inside the page
        // (same-origin, cookie attached) to hand Rust the raw response body
        // rather than the DOM's rendering of it.

        @JvmStatic
        fun webViewFetch(url: String, requestId: Long) {
            val app = instance
            if (app == null) {
                nativeWebFetchDone(requestId, "", "app not ready")
                return
            }
            Handler(Looper.getMainLooper()).post {
                try {
                    val wv = android.webkit.WebView(app)
                    wv.settings.javaScriptEnabled = true
                    wv.settings.domStorageEnabled = true
                    val handler = Handler(Looper.getMainLooper())
                    var finished = false
                    var polls = 0
                    fun finish(content: String, error: String) {
                        if (finished) return
                        finished = true
                        try { wv.destroy() } catch (_: Throwable) {}
                        try {
                            nativeWebFetchDone(requestId, content, error)
                        } catch (e: Throwable) {
                            android.util.Log.e("VerbaApp", "webViewFetch callback failed", e)
                        }
                    }
                    // Probe the page: report the challenge interstitial, then once
                    // clear, kick off a same-origin fetch of the URL and poll for
                    // its body. evaluateJavascript returns a JSON-encoded string.
                    val probeJs = """
                        (function () {
                          try {
                            var t = document.title || '';
                            if (t.indexOf('Just a moment') !== -1 ||
                                document.querySelector('#challenge-form,#challenge-running,#challenge-error-text')) {
                              return '__CHALLENGE__';
                            }
                            if (window.__verbaBody !== undefined) { return window.__verbaBody; }
                            if (!window.__verbaStarted) {
                              window.__verbaStarted = 1;
                              fetch(location.href, { credentials: 'include' })
                                .then(function (r) { return r.text(); })
                                .then(function (t) { window.__verbaBody = t; })
                                .catch(function (e) { window.__verbaBody = '__ERR__' + e; });
                            }
                            return '__WAIT__';
                          } catch (e) { return '__ERR__' + e; }
                        })()
                    """.trimIndent()
                    lateinit var poll: Runnable
                    poll = Runnable {
                        if (finished) return@Runnable
                        polls++
                        // ~40s at 700ms a step; Rust gives up at 45s regardless.
                        if (polls > 55) {
                            finish("", "timed out waiting for the site")
                            return@Runnable
                        }
                        wv.evaluateJavascript(probeJs) { raw ->
                            if (finished) return@evaluateJavascript
                            val s = try {
                                org.json.JSONTokener(raw ?: "null").nextValue() as? String
                            } catch (_: Throwable) { null }
                            when {
                                s == null || s == "__WAIT__" || s == "__CHALLENGE__" ->
                                    handler.postDelayed(poll, 700)
                                s.startsWith("__ERR__") -> finish("", s.removePrefix("__ERR__"))
                                else -> finish(s, "")
                            }
                        }
                    }
                    wv.webViewClient = object : android.webkit.WebViewClient() {
                        override fun onPageFinished(view: android.webkit.WebView?, u: String?) {
                            if (!finished) {
                                handler.removeCallbacks(poll)
                                handler.postDelayed(poll, 400)
                            }
                        }
                    }
                    // Safety net in case onPageFinished never fires.
                    handler.postDelayed(poll, 2500)
                    wv.loadUrl(url)
                } catch (e: Throwable) {
                    nativeWebFetchDone(requestId, "", e.message ?: "webview error")
                }
            }
        }

        @JvmStatic external fun nativeTtsPause()
        @JvmStatic external fun nativeTtsResume()
        @JvmStatic external fun nativeTtsStop()
        @JvmStatic external fun nativeTtsSeek(positionMs: Long)
        // Inbound share-target: text/URL shared to the app from elsewhere.
        @JvmStatic external fun nativeSharedText(text: String)
        // Selection-toolbar action: a word the user flagged as mispronounced.
        @JvmStatic external fun nativeReportMispronunciation(text: String)
        // Hidden-WebView fetch completion (body or error for a request id).
        @JvmStatic external fun nativeWebFetchDone(requestId: Long, content: String, error: String)
    }

    override fun onCreate() {
        super.onCreate()
        instance = this
        try {
            System.loadLibrary("verba_rs_lib")
        } catch (e: Throwable) {
            nativeLoadError = "${e::class.java.name}: ${e.message}"
            android.util.Log.e("VerbaApp", "Failed to load native library", e)
        }
    }
}
