// chdlite's Android activity, in place of the one `dx` generates (Dioxus.toml:
// `android_main_activity`). Three jobs on top of Dioxus' WryActivity:
//
//  * the controller: its buttons become keys the page understands (src/gui/android.rs, NAV_JS);
//  * All files access, asked for through ChdliteBridge so the Rust side can trigger it;
//  * ChdliteService, the foreground service with a progress notification and a partial wake
//    lock that keeps a long conversion running with the screen off or the app in the
//    background. Rust starts, updates and stops it through ChdliteBridge (src/android.rs).
//
// One file because dx takes exactly one custom Kotlin source.

package dev.dioxus.main

import android.Manifest
import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.os.IBinder
import android.os.PowerManager
import android.provider.Settings
import android.view.KeyEvent
import android.view.View
import android.view.WindowInsets
import androidx.annotation.Keep

// The generated classes refer to BuildConfig in this package. Must match Dioxus.toml's
// `[bundle] identifier`.
typealias BuildConfig = com.desertdoggy.chdlite.BuildConfig

class MainActivity : WryActivity() {
    // Back is the page's: it closes the topmost sheet or overlay (src/bin/chdlite-gui.rs).
    override val handleBackNavigation: Boolean = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Targeting Android 15 draws the app edge to edge, under the status and navigation
        // bars: without this the top bar sat under the clock. The page keeps the system's
        // default inset handling; only the content is moved in by the bars and any cutout.
        // Behind the bars: the page's own background (app.css), so the light status-bar icons
        // stay readable.
        window.decorView.setBackgroundColor(0xFF14161B.toInt())
        window.decorView.setOnApplyWindowInsetsListener { view, insets ->
            val content = findViewById<View>(android.R.id.content)
            if (Build.VERSION.SDK_INT >= 30) {
                val bars = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
                content.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            } else {
                @Suppress("DEPRECATION")
                content.setPadding(insets.systemWindowInsetLeft, insets.systemWindowInsetTop, insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
            }
            view.onApplyWindowInsets(insets)
        }
        // Android 13+: without this the run's notification is not shown (the service still runs).
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 2)
        }
    }

    // A = Enter, B and Back = Escape, Start = F2, Select = F3. The page does the rest.
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val mapped = when (event.keyCode) {
            KeyEvent.KEYCODE_BUTTON_A, KeyEvent.KEYCODE_DPAD_CENTER -> KeyEvent.KEYCODE_ENTER
            KeyEvent.KEYCODE_BUTTON_B, KeyEvent.KEYCODE_BACK -> KeyEvent.KEYCODE_ESCAPE
            KeyEvent.KEYCODE_BUTTON_START -> KeyEvent.KEYCODE_F2
            KeyEvent.KEYCODE_BUTTON_SELECT -> KeyEvent.KEYCODE_F3
            else -> return super.dispatchKeyEvent(event)
        }
        return super.dispatchKeyEvent(
            KeyEvent(event.downTime, event.eventTime, event.action, mapped, event.repeatCount,
                event.metaState, event.deviceId, event.scanCode, event.flags, event.source)
        )
    }
}

/**
 * Called from Rust (src/android.rs) through the activity's class loader. Nothing in Java or
 * Kotlin calls it, so a release build's R8 would drop it (Rust then got "Java exception was
 * thrown" from loadClass) without @Keep, which the default ProGuard rules honour.
 */
@Keep
object ChdliteBridge {
    @JvmStatic
    fun hasAllFilesAccess(context: Context): Boolean =
        if (Build.VERSION.SDK_INT >= 30) {
            Environment.isExternalStorageManager()
        } else {
            context.checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED
        }

    @JvmStatic
    fun requestAllFilesAccess(activity: Activity) {
        activity.runOnUiThread {
            if (Build.VERSION.SDK_INT >= 30) {
                try {
                    activity.startActivity(
                        Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:" + activity.packageName))
                    )
                } catch (e: Exception) {
                    activity.startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                }
            } else {
                activity.requestPermissions(
                    arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE, Manifest.permission.WRITE_EXTERNAL_STORAGE), 1
                )
            }
        }
    }

    @JvmStatic
    fun startRun(context: Context, title: String) {
        val intent = Intent(context, ChdliteService::class.java).putExtra(ChdliteService.EXTRA_TITLE, title)
        if (Build.VERSION.SDK_INT >= 26) context.startForegroundService(intent) else context.startService(intent)
    }

    @JvmStatic
    fun updateRun(context: Context, percent: Int, text: String) {
        ChdliteService.update(context, percent, text)
    }

    @JvmStatic
    fun stopRun(context: Context) {
        context.stopService(Intent(context, ChdliteService::class.java))
    }
}

/**
 * Holds the process in the foreground while a run is going: a notification with the run's
 * progress, and a partial wake lock so the CPU keeps converting with the screen off.
 */
class ChdliteService : Service() {
    companion object {
        const val EXTRA_TITLE = "title"
        private const val CHANNEL = "chdlite-run"
        private const val ID = 1
        // A run longer than this is not expected; the lock is released at the latest then.
        private const val WAKE_LOCK_MS = 12L * 60 * 60 * 1000

        @Volatile private var running = false
        @Volatile private var title = "chdlite"

        private fun channel(context: Context) {
            if (Build.VERSION.SDK_INT >= 26) {
                val manager = context.getSystemService(NotificationManager::class.java)
                if (manager.getNotificationChannel(CHANNEL) == null) {
                    manager.createNotificationChannel(
                        NotificationChannel(CHANNEL, "Conversions", NotificationManager.IMPORTANCE_LOW)
                    )
                }
            }
        }

        private fun build(context: Context, percent: Int, text: String): Notification {
            channel(context)
            val open = PendingIntent.getActivity(
                context, 0,
                Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            )
            val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(context, CHANNEL)
                          else @Suppress("DEPRECATION") Notification.Builder(context)
            return builder
                .setSmallIcon(android.R.drawable.stat_sys_download)
                .setContentTitle(title)
                .setContentText(text)
                .setProgress(100, percent, false)
                .setOnlyAlertOnce(true)
                .setOngoing(true)
                .setContentIntent(open)
                .build()
        }

        fun update(context: Context, percent: Int, text: String) {
            if (!running) return
            context.getSystemService(NotificationManager::class.java).notify(ID, build(context, percent, text))
        }
    }

    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        title = intent?.getStringExtra(EXTRA_TITLE) ?: "chdlite"
        val notification = build(this, 0, "")
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(ID, notification)
        }
        running = true
        if (wakeLock == null) {
            wakeLock = getSystemService(PowerManager::class.java)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "chdlite:run")
                .apply { setReferenceCounted(false); acquire(WAKE_LOCK_MS) }
        }
        // A killed process is not restarted into a run it no longer has.
        return START_NOT_STICKY
    }

    // Android 15 limits a dataSync service to six hours a day; past that the run goes on
    // without the service, as it would in the background.
    override fun onTimeout(startId: Int, fgsType: Int) {
        stopSelf()
    }

    override fun onDestroy() {
        running = false
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        if (Build.VERSION.SDK_INT >= 24) stopForeground(STOP_FOREGROUND_REMOVE) else @Suppress("DEPRECATION") stopForeground(true)
        super.onDestroy()
    }
}
