package com.alexb151.verba

import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Handler
import android.os.Looper

class VerbaApp : Application() {
    companion object {
        var instance: VerbaApp? = null
        var nativeLoadError: String? = null

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

        /// Open the system share sheet for a plain-text URL.
        @JvmStatic
        fun shareText(text: String) {
            val app = instance ?: return
            Handler(Looper.getMainLooper()).post {
                val intent = android.content.Intent(android.content.Intent.ACTION_SEND).apply {
                    type = "text/plain"
                    putExtra(android.content.Intent.EXTRA_TEXT, text)
                    addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
                }
                app.startActivity(
                    android.content.Intent.createChooser(intent, "Share link")
                        .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
                )
            }
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

        // Inbound share-target: text/URL shared to the app from elsewhere.
        @JvmStatic external fun nativeSharedText(text: String)
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
