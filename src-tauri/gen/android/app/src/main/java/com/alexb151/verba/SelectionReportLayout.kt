package com.alexb151.verba

import android.content.Context
import android.graphics.Rect
import android.view.ActionMode
import android.view.Menu
import android.view.MenuItem
import android.view.View
import android.webkit.WebView
import android.widget.FrameLayout
import android.widget.Toast
import org.json.JSONTokener

private const val REPORT_MENU_ID = 0x1EAD // arbitrary id, unlikely to clash with Chromium's

/**
 * Wraps the Tauri WebView so we can inject a "Report mispronunciation" item into
 * the text-selection floating toolbar and, crucially, KEEP it there.
 *
 * The WebView starts its selection action mode via View.startActionMode(), which
 * calls this parent's startActionModeForChild(). We wrap Chromium's callback:
 * every time the menu is (re)built in onPrepareActionMode — async TextClassifier
 * results, each selection-handle move — we re-add our item. A one-shot add via
 * Activity.onActionModeStarted got wiped by those rebuilds, which is why the item
 * showed on some selections and not others (even the same word).
 *
 * This lives at the parent level on purpose: RustWebView is auto-generated and
 * gitignored, so we cannot subclass it to override startActionMode directly.
 */
class SelectionReportLayout(context: Context) : FrameLayout(context) {
    var webView: WebView? = null

    override fun startActionModeForChild(
        originalView: View,
        callback: ActionMode.Callback,
        type: Int
    ): ActionMode? = super.startActionModeForChild(originalView, wrap(callback), type)

    // Callback2 (not plain Callback) so onGetContentRect is preserved — that rect
    // positions the floating toolbar over the selection.
    private fun wrap(inner: ActionMode.Callback): ActionMode.Callback =
        object : ActionMode.Callback2() {
            override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean =
                inner.onCreateActionMode(mode, menu)

            override fun onPrepareActionMode(mode: ActionMode, menu: Menu): Boolean {
                val changed = inner.onPrepareActionMode(mode, menu)
                // Re-add after Chromium has (re)populated the menu. Text-selection
                // (floating) toolbar only, and de-dup in case the menu was not cleared.
                if (mode.type == ActionMode.TYPE_FLOATING) {
                    menu.removeItem(REPORT_MENU_ID)
                    if (!VerbaApp.reportMenuEnabled) return changed
                    menu.add(Menu.NONE, REPORT_MENU_ID, Menu.CATEGORY_SECONDARY, "Report mispronunciation")
                        // Handle the click on the item itself. Routing through the wrapped
                        // callback's onActionItemClicked fired for the smart-selection menu
                        // (with "Define") but not the basic Copy/Select-all/Share one.
                        // MenuBuilder always invokes this listener, whichever menu it is.
                        .setOnMenuItemClickListener {
                            report(mode)
                            true
                        }
                    return true
                }
                return changed
            }

            override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean =
                inner.onActionItemClicked(mode, item)

            override fun onDestroyActionMode(mode: ActionMode) = inner.onDestroyActionMode(mode)

            override fun onGetContentRect(mode: ActionMode, view: View?, outRect: Rect) {
                if (inner is ActionMode.Callback2) inner.onGetContentRect(mode, view, outRect)
                else super.onGetContentRect(mode, view, outRect)
            }
        }

    private fun report(mode: ActionMode) {
        val wv = webView
        if (wv == null || VerbaApp.nativeLoadError != null) {
            mode.finish()
            return
        }
        // Read the selection BEFORE finishing the mode. finish() clears the DOM
        // selection, and evaluateJavascript is async — finishing first raced the read
        // and returned empty (so the report silently did nothing). Decode the
        // JSON-encoded result, then finish inside the callback.
        wv.evaluateJavascript("(window.getSelection && window.getSelection().toString()) || ''") { raw ->
            val word = (JSONTokener(raw).nextValue() as? String)?.trim().orEmpty()
            if (word.isNotEmpty()) {
                try {
                    VerbaApp.nativeReportMispronunciation(word)
                    val shown = if (word.length > 24) word.take(24) + "…" else word
                    Toast.makeText(context, "Reported: $shown", Toast.LENGTH_SHORT).show()
                } catch (e: Throwable) {
                    android.util.Log.e("SelectionReportLayout", "report failed", e)
                }
            }
            mode.finish()
        }
    }
}
