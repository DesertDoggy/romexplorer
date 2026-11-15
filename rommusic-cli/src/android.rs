//! What the Android app needs from Java: its own directories, All files access, and the
//! foreground service that keeps a long run alive.
//!
//! The Java half is `rommusic-cli/android/MainActivity.kt` (`RommusicBridge`, `RommusicService`),
//! which `dx` compiles into the APK in place of its default activity (see `Dioxus.toml`).
//! Every call here is best-effort: a failure is logged and the caller carries on with a
//! fallback, because none of this is worth refusing to start over.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jni::objects::{JClass, JObject, JString, JValue};
use jni::{AttachGuard, JNIEnv, JavaVM};

const BRIDGE: &str = "dev.dioxus.main.RommusicBridge";

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

/// Set by the app (`rommusic-gui`) first thing: the command-line tools on Android run outside
/// any app, with no Java to ask, and must never touch `ndk_context` (it panics there).
static IN_APP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn mark_app() {
    IN_APP.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether this process is the Android app rather than a command-line tool.
pub fn is_app() -> bool {
    IN_APP.load(std::sync::atomic::Ordering::Relaxed)
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
    // The exception itself (class and message), not only jni's "Java exception was thrown".
    let mut thrown = None;
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        if let Ok(exception) = env.exception_occurred() {
            let _ = env.exception_clear();
            thrown = env
                .call_method(&exception, "toString", "()Ljava/lang/String;", &[])
                .and_then(|text| text.l())
                .and_then(|text| string(&mut env, text))
                .ok();
        }
        let _ = env.exception_clear();
    }
    result.map_err(|err| match thrown {
        Some(text) => format!("{err}: {text}"),
        None => err.to_string(),
    })
}

/// `RommusicBridge`, through the activity's class loader: `FindClass` on a native thread sees
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

/// The app's private files directory (`Context.getFilesDir()`): where `rommusic-gui.json` lives.
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
            Err(_) => PathBuf::from("/data/data").join(package_name().unwrap_or_else(|| "com.desertdoggy.rommusic".to_string())).join("files"),
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
        .find(|path| path.ends_with("/libmain.so") || path.ends_with("/librommusic_gui.so"))
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

/// Whether rommusic may read and write shared storage as plain paths (All files access on
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
        eprintln!("rommusic: foreground service not started: {err}");
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

/// Copies the files of the APK's `assets/` whose names `wanted` picks into `dir`, each written
/// beside its place and renamed, so a copy cut short is never read as whole. Returns how many
/// were copied. (APK assets are not files on disk: they are read through the AssetManager.)
pub fn copy_assets(wanted: impl Fn(&str) -> bool, dir: &Path) -> Result<usize, String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    with_env(|env, activity| {
        let assets = env.call_method(activity, "getAssets", "()Landroid/content/res/AssetManager;", &[])?.l()?;
        let root = env.new_string("")?;
        let names = env.call_method(&assets, "list", "(Ljava/lang/String;)[Ljava/lang/String;", &[JValue::from(&root)])?.l()?;
        let names = jni::objects::JObjectArray::from(names);
        let count = env.get_array_length(&names)?;
        let buffer = env.new_byte_array(1 << 16)?;
        let mut chunk = vec![0i8; 1 << 16];
        let mut copied = 0;
        for i in 0..count {
            let name = env.get_object_array_element(&names, i)?;
            let name = string(env, name)?;
            if !wanted(&name) {
                continue;
            }
            let jname = env.new_string(&name)?;
            let stream = env.call_method(&assets, "open", "(Ljava/lang/String;)Ljava/io/InputStream;", &[JValue::from(&jname)])?.l()?;
            let target = dir.join(&name);
            let part = dir.join(format!("{name}.part"));
            let written = (|| -> Result<(), String> {
                let mut out = std::fs::File::create(&part).map_err(|err| format!("{}: {err}", part.display()))?;
                loop {
                    let read = env.call_method(&stream, "read", "([B)I", &[JValue::from(&buffer)]).and_then(|r| r.i()).map_err(|err| format!("{name}: {err}"))?;
                    if read < 0 {
                        break;
                    }
                    let read = read as usize;
                    env.get_byte_array_region(&buffer, 0, &mut chunk[..read]).map_err(|err| format!("{name}: {err}"))?;
                    // SAFETY: i8 and u8 have the same size and alignment.
                    let bytes = unsafe { std::slice::from_raw_parts(chunk.as_ptr().cast::<u8>(), read) };
                    out.write_all(bytes).map_err(|err| format!("{}: {err}", part.display()))?;
                }
                drop(out);
                std::fs::rename(&part, &target).map_err(|err| format!("{}: {err}", target.display()))
            })();
            let _ = env.call_method(&stream, "close", "()V", &[]);
            env.delete_local_ref(stream)?;
            env.delete_local_ref(jname)?;
            match written {
                Ok(()) => copied += 1,
                Err(err) => {
                    let _ = std::fs::remove_file(&part);
                    log::warn!("APK asset {err}");
                }
            }
        }
        Ok(copied)
    })
}
