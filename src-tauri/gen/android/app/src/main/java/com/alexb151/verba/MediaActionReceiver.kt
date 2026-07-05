package com.alexb151.verba

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationManagerCompat

/**
 * Handles taps on the reader notification's action buttons.
 *
 * The buttons used to be built with MediaButtonReceiver.buildMediaButtonPendingIntent,
 * which returns null when the manifest declares no MEDIA_BUTTON receiver or media
 * service — so they rendered but did nothing. Declaring that receiver without a
 * companion media service is worse: its onReceive throws. Explicit app-private
 * broadcasts to this receiver sidestep the whole mechanism. Hardware/Bluetooth
 * buttons and system media surfaces still go through the MediaSession callback,
 * unchanged.
 */
class MediaActionReceiver : BroadcastReceiver() {
    companion object {
        const val ACTION_PLAY_PAUSE = "com.alexb151.verba.media.PLAY_PAUSE"
        const val ACTION_REWIND = "com.alexb151.verba.media.REWIND"
        const val ACTION_FAST_FORWARD = "com.alexb151.verba.media.FAST_FORWARD"
        const val ACTION_EXIT = "com.alexb151.verba.media.EXIT"
    }

    override fun onReceive(context: Context, intent: Intent) {
        // No live session behind this notification (e.g. it outlived a process
        // death, or the native lib failed to load): there is nothing left to
        // control, so just take the orphaned notification down.
        if (VerbaApp.nativeLoadError != null || !VerbaApp.hasMediaSession()) {
            NotificationManagerCompat.from(context).cancel(VerbaApp.NOTIFICATION_ID)
            return
        }
        try {
            when (intent.action) {
                ACTION_PLAY_PAUSE ->
                    if (VerbaApp.lastPaused) VerbaApp.nativeTtsResume() else VerbaApp.nativeTtsPause()
                ACTION_REWIND -> VerbaApp.nativeTtsSeek(maxOf(0, VerbaApp.lastPositionMs - 15000))
                ACTION_FAST_FORWARD -> VerbaApp.nativeTtsSeek(VerbaApp.lastPositionMs + 15000)
                ACTION_EXIT -> VerbaApp.nativeTtsStop()
            }
        } catch (e: Throwable) {
            android.util.Log.e("MediaActionReceiver", "media action failed", e)
        }
    }
}
