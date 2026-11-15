// romcompanion's Android activity, in place of the one `dx` generates (Dioxus.toml:
// `android_main_activity`). Two jobs on top of Dioxus' WryActivity, the same as chdlite's:
//
//  * Back is the page's. WryActivity's own handling left the app (the router keeps no
//    WebView history), so Back now reaches Big Picture as Escape: it closes the open sheet or
//    menu, then goes back a page (romexplorer-gui/src/screens/bigpicture.rs, NAV_JS).
//  * The system bars: targeting Android 15 draws edge to edge, which put the sheets' buttons
//    under the navigation bar. The content is moved in by the bars and any cutout instead.
//
// A controller's A, B, Start and Select become the keys Big Picture already understands; the
// rest (D-pad, X, Y, shoulders) reach it through the WebView's Gamepad API as before.
//
// One file because dx takes exactly one custom Kotlin source.

package dev.dioxus.main

import android.os.Build
import android.os.Bundle
import android.view.KeyEvent
import android.view.View
import android.view.WindowInsets

// The generated classes refer to BuildConfig in this package. Must match Dioxus.toml's
// `[bundle] identifier`.
typealias BuildConfig = com.desertdoggy.romcompanion.BuildConfig

class MainActivity : WryActivity() {
    override val handleBackNavigation: Boolean = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Behind the bars: the app's own background (app.css --bg), so the light status-bar
        // icons stay readable.
        window.decorView.setBackgroundColor(0xFF14161A.toInt())
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
    }

    // A = Enter, B and Back = Escape, Start = / (search), Select = f (filter): Big Picture's
    // keyboard keys for the same buttons.
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val mapped = when (event.keyCode) {
            KeyEvent.KEYCODE_BUTTON_A, KeyEvent.KEYCODE_DPAD_CENTER -> KeyEvent.KEYCODE_ENTER
            KeyEvent.KEYCODE_BUTTON_B, KeyEvent.KEYCODE_BACK -> KeyEvent.KEYCODE_ESCAPE
            KeyEvent.KEYCODE_BUTTON_START -> KeyEvent.KEYCODE_SLASH
            KeyEvent.KEYCODE_BUTTON_SELECT -> KeyEvent.KEYCODE_F
            else -> return super.dispatchKeyEvent(event)
        }
        return super.dispatchKeyEvent(
            KeyEvent(event.downTime, event.eventTime, event.action, mapped, event.repeatCount,
                event.metaState, event.deviceId, event.scanCode, event.flags, event.source)
        )
    }
}
