package com.alexb151.verba

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.View
import android.webkit.WebView
import android.widget.FrameLayout
import android.widget.TextView
import android.widget.ScrollView
import android.util.TypedValue
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    val err = VerbaApp.nativeLoadError
    if (err != null) {
      super.onCreate(savedInstanceState)
      val sv = ScrollView(this)
      val tv = TextView(this)
      tv.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
      tv.setPadding(32, 100, 32, 32)
      tv.text = "Native library failed to load:\n\n$err"
      sv.addView(tv)
      setContentView(sv)
      return
    }
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    requestNotificationPermission()
    // Cold start via a share: the native lib is loaded here, but the webview
    // (and its `shared-text` listener) isn't ready yet, so the text is stashed
    // in Rust and the frontend pulls it once it initializes.
    handleSharedIntent(intent)
  }

  // singleTask launch mode: a share while the app is already running arrives
  // here rather than as a fresh onCreate.
  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    setIntent(intent)
    handleSharedIntent(intent)
  }

  private fun handleSharedIntent(intent: Intent?) {
    if (VerbaApp.nativeLoadError != null) return
    if (intent?.action != Intent.ACTION_SEND) return
    if (intent.type?.startsWith("text/") != true) return
    val text = intent.getStringExtra(Intent.EXTRA_TEXT) ?: return
    if (text.isBlank()) return
    try {
      VerbaApp.nativeSharedText(text)
    } catch (e: Throwable) {
      android.util.Log.e("MainActivity", "nativeSharedText failed", e)
    }
  }

  private fun requestNotificationPermission() {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
      if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
        requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1001)
      }
    }
  }

  // Wry calls setContentView(webView) directly (main_pipe.rs). Slip the WebView
  // into a wrapper ViewGroup whose startActionModeForChild we control, so we can
  // add — and KEEP — a "Report mispronunciation" item in the text-selection
  // toolbar. Adding it once via onActionModeStarted was unreliable: Chromium
  // rebuilds the selection menu asynchronously (TextClassifier results, every
  // selection-handle move), wiping a one-shot add. The wrapper re-adds on every
  // rebuild. RustWebView is auto-generated and gitignored, so we cannot subclass
  // it to override startActionMode directly — the parent hook is the way in.
  override fun setContentView(view: View?) {
    if (view is WebView) {
      val wrapper = SelectionReportLayout(this)
      wrapper.webView = view
      wrapper.addView(
        view,
        FrameLayout.LayoutParams(
          FrameLayout.LayoutParams.MATCH_PARENT,
          FrameLayout.LayoutParams.MATCH_PARENT
        )
      )
      super.setContentView(wrapper)
    } else {
      super.setContentView(view)
    }
  }
}
