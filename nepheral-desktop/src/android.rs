// SPDX-License-Identifier: BUSL-1.1
//! Calls into the app's own Kotlin classes (`KeySealer`, `AppSource`,
//! `CustodySwitch`).
//!
//! The classes are looked up through the app's class loader: from a thread
//! Rust started, JNI's default one sees only the system's classes.

use jni::objects::{JClass, JObject, JValue};
use jni::JavaVM;

/// Runs `f` with the class `name` and the application's context.
pub fn with_class<R>(
    name: &str,
    f: impl FnOnce(&mut jni::JNIEnv, &JClass, &JObject) -> jni::errors::Result<R>,
) -> Result<R, String> {
    let ctx = ndk_context::android_context();
    // SAFETY: tao fills the Android context with the process's VM and a
    // global reference to the application before the app runs.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    // SAFETY: a global reference that lives as long as the process; only
    // borrowed here, never deleted.
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result = (|| {
        let loader = env
            .call_method(&context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
            .l()?;
        let jname = env.new_string(name)?;
        let class = env
            .call_method(
                &loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[JValue::Object(&jname)],
            )?
            .l()?;
        let class = JClass::from(class);
        f(&mut env, &class, &context)
    })();
    result.map_err(|e| {
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        format!("{name}: {e}")
    })
}

/// Installed from Google Play: it updates through Play only.
pub fn installed_from_play() -> bool {
    with_class("net.nepheral.app.AppSource", |env, class, context| {
        env.call_static_method(
            class,
            "fromPlay",
            "(Landroid/content/Context;)Z",
            &[JValue::Object(context)],
        )?
        .z()
    })
    .unwrap_or(false)
}

/// Whether the phone helps the network in the background.
pub fn custody_background() -> bool {
    with_class("net.nepheral.app.CustodySwitch", |env, class, context| {
        env.call_static_method(
            class,
            "isOn",
            "(Landroid/content/Context;)Z",
            &[JValue::Object(context)],
        )?
        .z()
    })
    .unwrap_or(true)
}

/// Turns background help on (starting the foreground service) or off
/// (stopping it), and remembers the choice.
pub fn set_custody_background(on: bool) -> Result<(), String> {
    with_class("net.nepheral.app.CustodySwitch", |env, class, context| {
        env.call_static_method(
            class,
            "set",
            "(Landroid/content/Context;Z)V",
            &[JValue::Object(context), JValue::Bool(u8::from(on))],
        )?;
        Ok(())
    })
}

/// Opens `url` in the browser.
pub fn open_url(url: &str) -> Result<(), String> {
    with_class("net.nepheral.app.AppSource", |env, class, context| {
        let u = env.new_string(url)?;
        env.call_static_method(
            class,
            "open",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[JValue::Object(context), JValue::Object(&u)],
        )?;
        Ok(())
    })
}
