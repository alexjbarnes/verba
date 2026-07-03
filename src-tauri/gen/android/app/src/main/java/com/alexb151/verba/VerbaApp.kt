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
import androidx.media.session.MediaButtonReceiver

class VerbaApp : Application() {
    companion object {
        var instance: VerbaApp? = null
        var nativeLoadError: String? = null

        private const val CHANNEL_ID = "verba_tts"
        private const val NOTIFICATION_ID = 42

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
        private var lastPositionMs: Long = 0
        private var sessionActive = false

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
                })
                session.isActive = true
                mediaSession = session
                showNotification(app, false)
            }
        }

        @JvmStatic
        fun updateMediaSession(positionMs: Long, durationMs: Long, paused: Boolean) {
            lastPositionMs = positionMs
            val session = mediaSession ?: return
            val state = PlaybackStateCompat.Builder()
                .setActions(
                    PlaybackStateCompat.ACTION_PLAY or
                    PlaybackStateCompat.ACTION_PAUSE or
                    PlaybackStateCompat.ACTION_STOP or
                    PlaybackStateCompat.ACTION_SEEK_TO or
                    PlaybackStateCompat.ACTION_PLAY_PAUSE
                )
                .setState(
                    if (paused) PlaybackStateCompat.STATE_PAUSED else PlaybackStateCompat.STATE_PLAYING,
                    positionMs,
                    1f
                )
                .build()
            session.setPlaybackState(state)

            val metadata = MediaMetadataCompat.Builder()
                .putString(MediaMetadataCompat.METADATA_KEY_TITLE, "Verba Reader")
                .putLong(MediaMetadataCompat.METADATA_KEY_DURATION, durationMs)
                .build()
            session.setMetadata(metadata)

            val app = instance ?: return
            showNotification(app, paused)
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
                    AudioManager.AUDIOFOCUS_LOSS,
                    AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> nativeTtsPause()
                    AudioManager.AUDIOFOCUS_GAIN -> nativeTtsResume()
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

            val playPauseAction = if (paused) {
                NotificationCompat.Action.Builder(
                    android.R.drawable.ic_media_play, "Play",
                    MediaButtonReceiver.buildMediaButtonPendingIntent(ctx, PlaybackStateCompat.ACTION_PLAY)
                ).build()
            } else {
                NotificationCompat.Action.Builder(
                    android.R.drawable.ic_media_pause, "Pause",
                    MediaButtonReceiver.buildMediaButtonPendingIntent(ctx, PlaybackStateCompat.ACTION_PAUSE)
                ).build()
            }

            val stopAction = NotificationCompat.Action.Builder(
                android.R.drawable.ic_menu_close_clear_cancel, "Stop",
                MediaButtonReceiver.buildMediaButtonPendingIntent(ctx, PlaybackStateCompat.ACTION_STOP)
            ).build()

            val notification = NotificationCompat.Builder(ctx, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.ic_lock_silent_mode_off)
                .setContentTitle("Verba Reader")
                .setContentIntent(contentIntent)
                .addAction(playPauseAction)
                .addAction(stopAction)
                .setStyle(
                    androidx.media.app.NotificationCompat.MediaStyle()
                        .setMediaSession(session.sessionToken)
                        .setShowActionsInCompactView(0, 1)
                )
                .setOngoing(!paused)
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
