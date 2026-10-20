//! Android owns document picking and audio focus; only owned file paths cross into the core.
use jni::{
    objects::{GlobalRef, JObject, JString, JValue},
    sys::jboolean,
    JNIEnv, JavaVM,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
};

struct Activity {
    vm: JavaVM,
    object: GlobalRef,
}
static ACTIVITY: Mutex<Option<Activity>> = Mutex::new(None);
static DIRECTORIES: OnceLock<()> = OnceLock::new();
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static IMPORTS: Mutex<Vec<ImportEvent>> = Mutex::new(Vec::new());

pub enum ImportEvent {
    File,
    Error(String),
    Finished,
}

pub fn init(app: &slint::android::AndroidApp) -> Result<PathBuf, String> {
    let root = app
        .internal_data_path()
        .ok_or("Android did not provide private storage")?;
    // Set process paths once, before our core workers start, not again on activity recreation.
    DIRECTORIES.get_or_init(|| unsafe {
        std::env::set_var("HOME", &root);
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_DATA_HOME", root.join("data"));
        std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
    });
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
    let object = {
        let env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        env.new_global_ref(activity).map_err(|e| e.to_string())?
    };
    // NativeActivity can enter Rust again in the same process after its window is recreated.
    *ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()) = Some(Activity { vm, object });
    Ok(root)
}

fn call(name: &str, signature: &str, args: &[JValue<'_, '_>]) -> Result<bool, String> {
    let activity = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
    let a = activity.as_ref().ok_or("Android activity is unavailable")?;
    let mut env = a.vm.attach_current_thread().map_err(|e| e.to_string())?;
    let result = env.call_method(a.object.as_obj(), name, signature, args);
    match result {
        Ok(v) => v.z().map_err(|e| e.to_string()),
        Err(e) => {
            let _ = env.exception_clear();
            Err(format!("Android {name}: {e}"))
        }
    }
}

pub fn import() -> Result<(), String> {
    if call("pickMusic", "()Z", &[])? {
        Ok(())
    } else {
        Err("Music picker is already open".into())
    }
}
pub fn set_playing(on: bool) -> Result<(), String> {
    if call("playback", "(Z)Z", &[JValue::Bool(on as jboolean)])? {
        Ok(())
    } else {
        Err("Another app owns audio focus. Pause it, then press Play.".into())
    }
}
pub fn background() -> Result<(), String> {
    if call("background", "()Z", &[])? {
        Ok(())
    } else {
        Err("Android could not move the app to the background".into())
    }
}
pub fn interrupt() {
    INTERRUPTED.store(true, Ordering::Release);
}
pub fn take_interrupted() -> bool {
    INTERRUPTED.swap(false, Ordering::AcqRel)
}
pub fn drain_imports() -> Vec<ImportEvent> {
    std::mem::take(&mut *IMPORTS.lock().unwrap_or_else(|e| e.into_inner()))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_openfader_mobile_OpenFaderActivity_nativeInterrupted(
    _env: JNIEnv,
    _this: JObject,
) {
    interrupt();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_openfader_mobile_OpenFaderActivity_nativeImported(
    mut env: JNIEnv,
    _this: JObject,
    error: JString,
) {
    let event = if error.is_null() {
        ImportEvent::File
    } else {
        match env.get_string(&error) {
            Ok(s) => ImportEvent::Error(s.into()),
            Err(_) => ImportEvent::Error("Unable to read import error".into()),
        }
    };
    IMPORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(event);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_openfader_mobile_OpenFaderActivity_nativeImportFinished(
    _env: JNIEnv,
    _this: JObject,
) {
    IMPORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(ImportEvent::Finished);
}
