//! What the Android app needs from Java: its own directories, All files access, and the
//! foreground service that keeps a long conversion alive.
//!
//! The Java half is `chdlite/android/MainActivity.kt` (`ChdliteBridge`, `ChdliteService`),
//! which `dx` compiles into the APK in place of its default activity (see `Dioxus.toml`).
//! Every call here is best-effort: a failure is logged and the caller carries on with a
//! fallback, because none of this is worth refusing to start over.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::{AttachGuard, JNIEnv, JavaVM};

const BRIDGE: &str = "dev.dioxus.main.ChdliteBridge";

/// The application id from `/proc/self/cmdline` (Android writes the package name there), for
/// the fallbacks when Java cannot be reached.
fn package_name() -> Option<String> {
    let raw = std::fs::read("/proc/self/cmdline").ok()?;
    let name = raw.split(|b| *b == 0).next()?;
    let name = String::from_utf8_lossy(name).trim().to_string();
    // A `:service` suffix names a process, not the package.
    let name = name.split(':').next().unwrap_or("").to_string();
    (!name.is_empty()).then_some(name)
}

fn vm() -> Result<JavaVM, String> {
    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() {
        return Err("no Java VM yet".to_string());
    }
    // SAFETY: ndk_context holds the process's JavaVM pointer, set by tao before `main` runs.
    unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|err| err.to_string())
}

/// The activity, as ndk_context holds it (a global reference it owns: never deleted here).
fn activity<'a>() -> Result<JObject<'a>, String> {
    let ptr = ndk_context::android_context().context();
    if ptr.is_null() {
        return Err("no activity yet".to_string());
    }
    // SAFETY: a live global reference for as long as the activity exists.
    Ok(unsafe { JObject::from_raw(ptr.cast()) })
}

/// Runs `body` on an attached thread, turning a pending Java exception into an error.
fn with_env<T>(body: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Result<T, String> {
    let vm = vm()?;
    let mut env: AttachGuard = vm.attach_current_thread().map_err(|err| err.to_string())?;
    let activity = activity()?;
    let result = body(&mut env, &activity);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    result.map_err(|err| err.to_string())
}

/// `ChdliteBridge`, through the activity's class loader: `FindClass` on a native thread sees
/// only the system classes.
fn bridge<'a>(env: &mut JNIEnv<'a>, activity: &JObject) -> jni::errors::Result<JClass<'a>> {
    let loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
    let name = env.new_string(BRIDGE)?;
    let class = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::from(&name)])?.l()?;
    Ok(JClass::from(class))
}

fn string(env: &mut JNIEnv, object: JObject) -> jni::errors::Result<String> {
    let text = JString::from(object);
    let value: String = env.get_string(&text)?.into();
    Ok(value)
}

/// The app's private files directory (`Context.getFilesDir()`): where `chdlite.toml` lives.
pub fn files_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let found = with_env(|env, activity| {
            let dir = env.call_method(activity, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
            let path = env.call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[])?.l()?;
            string(env, path)
        });
        match found {
            Ok(path) => PathBuf::from(path),
            Err(_) => PathBuf::from("/data/data").join(package_name().unwrap_or_else(|| "com.desertdoggy.chdlite".to_string())).join("files"),
        }
    })
    .clone()
}

/// Where the APK's native libraries were extracted (`ApplicationInfo.nativeLibraryDir`).
pub fn native_lib_dir() -> Option<PathBuf> {
    let found = with_env(|env, activity| {
        let info = env.call_method(activity, "getApplicationInfo", "()Landroid/content/pm/ApplicationInfo;", &[])?.l()?;
        let dir = env.get_field(&info, "nativeLibraryDir", "Ljava/lang/String;")?.l()?;
        string(env, dir)
    });
    if let Ok(dir) = found {
        return Some(PathBuf::from(dir));
    }
    // Without Java: the folder this library itself was loaded from.
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .find(|path| path.ends_with("/libmain.so") || path.ends_with("/libchdlite_gui.so"))
        .and_then(|path| Path::new(path).parent().map(Path::to_path_buf))
}

/// The storage roots a user can browse: internal shared storage first, then every other
/// volume under `/storage` (SD cards and USB drives appear there as `XXXX-XXXX`).
pub fn storage_roots() -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    let internal = PathBuf::from("/storage/emulated/0");
    if internal.is_dir() {
        roots.push(("Internal storage".to_string(), internal));
    }
    if let Ok(entries) = std::fs::read_dir("/storage") {
        let mut volumes: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                name != "emulated" && name != "self" && path.is_dir()
            })
            .collect();
        volumes.sort();
        for volume in volumes {
            let name = volume.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            roots.push((format!("SD card / USB ({name})"), volume));
        }
    }
    roots
}

/// Whether chdlite may read and write shared storage as plain paths (All files access on
/// Android 11+, the storage permission before that).
pub fn has_file_access() -> bool {
    with_env(|env, activity| {
        let class = bridge(env, activity)?;
        env.call_static_method(&class, "hasAllFilesAccess", "(Landroid/content/Context;)Z", &[JValue::Object(activity)])?.z()
    })
    .unwrap_or(false)
}

/// Opens the system screen that grants All files access (or asks for the storage permission
/// on Android 10 and older). The answer arrives when the user comes back to the app.
pub fn request_file_access() -> Result<(), String> {
    with_env(|env, activity| {
        let class = bridge(env, activity)?;
        env.call_static_method(&class, "requestAllFilesAccess", "(Landroid/app/Activity;)V", &[JValue::Object(activity)])?;
        Ok(())
    })
}

/// Starts the foreground service (its notification and a partial wake lock), so Android
/// keeps the process running with the screen off or the app in the background.
pub fn run_started(title: &str) {
    let result = with_env(|env, activity| {
        let class = bridge(env, activity)?;
        let title = env.new_string(title)?;
        env.call_static_method(&class, "startRun", "(Landroid/content/Context;Ljava/lang/String;)V", &[JValue::Object(activity), JValue::from(&title)])?;
        Ok(())
    });
    if let Err(err) = result {
        eprintln!("chdlite: foreground service not started: {err}");
    }
}

/// Updates the notification: `percent` overall, and a line of text under it.
pub fn run_progress(percent: f32, text: &str) {
    let _ = with_env(|env, activity| {
        let class = bridge(env, activity)?;
        let text = env.new_string(text)?;
        env.call_static_method(&class, "updateRun", "(Landroid/content/Context;ILjava/lang/String;)V", &[JValue::Object(activity), JValue::Int(percent.clamp(0.0, 100.0) as i32), JValue::from(&text)])?;
        Ok(())
    });
}

/// Stops the service and releases the wake lock.
pub fn run_finished() {
    let _ = with_env(|env, activity| {
        let class = bridge(env, activity)?;
        env.call_static_method(&class, "stopRun", "(Landroid/content/Context;)V", &[JValue::Object(activity)])?;
        Ok(())
    });
}

/// Sends the app to the background, as Back does at an app's top level.
pub fn leave() {
    let _ = with_env(|env, activity| {
        env.call_method(activity, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)])?;
        Ok(())
    });
}
