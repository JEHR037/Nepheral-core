// SPDX-License-Identifier: BUSL-1.1
//! The Nepheral app: the desktop client, and, run with `Profile::Light`,
//! the light node a phone runs. Both are this code; the profile decides
//! what kind of node runs and which pages the window shows.

#[cfg(target_os = "android")]
mod android;
mod commands;
mod gate;
mod lock;
mod node_store;
#[cfg(windows)]
mod tpm;
mod updates;

use std::sync::OnceLock;

/// What kind of node the app runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// The desktop: runs a model, can be a validator, keeps the whole chain.
    Full,
    /// The phone: holds fragments and asks the network. No model, no blocks
    /// produced, and the chain followed from a light proof. Its pages speak
    /// of credits and AI only.
    Light,
}

static PROFILE: OnceLock<Profile> = OnceLock::new();

/// The profile this app was started with.
pub fn profile() -> Profile {
    PROFILE.get().copied().unwrap_or(Profile::Full)
}

pub fn is_light() -> bool {
    profile() == Profile::Light
}

/// Runs the app until its window closes. `context` comes from the calling
/// crate's `tauri::generate_context!()`, so each app keeps its own name,
/// identifier and icons.
pub fn run(context: tauri::Context<tauri::Wry>, profile: Profile) {
    let _ = PROFILE.set(profile);
    init_logging();

    tracing::info!(?profile, "Nepheral starting");

    // With a wallet, the node starts at once and the main window opens
    // locked; the first time, a small window creates the wallet first. See
    // gate::open_first_window.
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    builder
        .manage(gate::Gate::default())
        .invoke_handler(commands::handler())
        .setup(|app| {
            // A phone has no home directory: keep everything in the app's own.
            #[cfg(mobile)]
            {
                use tauri::Manager;
                if let Ok(dir) = app.path().app_data_dir() {
                    std::env::set_var("NEPHERAL_HOME", dir.join(".nepheral"));
                }
            }
            gate::open_first_window(app.handle())?;
            Ok(())
        })
        .build(context)
        .expect("tauri app failed to build")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(node) = gate::NODE.get() {
                    tracing::info!("Window closed — shutting down node tasks");
                    node.shutdown();
                    tracing::info!("Nepheral shut down gracefully");
                }
            }
        });
}

fn env_filter() -> tracing_subscriber::EnvFilter {
    utils::env_filter()
}

/// Logs go nowhere on disk by default. They held peer addresses and
/// identities in a plain file anyone could read; the activity view in the app
/// keeps what a user needs. NEPHERAL_LOG_FILE=1 writes the old file again,
/// for diagnosing a problem; any other value is taken as the path.
fn init_logging() {
    // On Android stdout and stderr go nowhere: logs and panics go to logcat.
    #[cfg(target_os = "android")]
    {
        logcat::install(env_filter());
        return;
    }
    #[allow(unreachable_code)]
    if let Ok(v) = std::env::var("NEPHERAL_LOG_FILE") {
        let v = v.trim();
        let path = if v.is_empty() || v == "1" {
            let dir = app_kernel::nepheral_config::nepheral_dir();
            let _ = std::fs::create_dir_all(&dir);
            dir.join(app_kernel::vault::DESKTOP_LOG_FILE)
        } else {
            std::path::PathBuf::from(v)
        };
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            tracing_subscriber::fmt()
                .with_env_filter(env_filter())
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false)
                .init();
            return;
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .init();
}

/// Android's log, read with `adb logcat -s Nepheral`. liblog is already
/// linked by the runtime.
#[cfg(target_os = "android")]
mod logcat {
    use std::ffi::{c_char, c_int, CString};
    use std::io::{self, Write};

    const INFO: c_int = 4;
    const FATAL: c_int = 7;

    #[link(name = "log")]
    extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    fn write(prio: c_int, msg: &str) {
        let tag = c"Nepheral";
        for line in msg.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(text) = CString::new(line.replace('\0', "")) {
                // SAFETY: both pointers are valid NUL-terminated strings for
                // the duration of the call.
                unsafe {
                    __android_log_write(prio, tag.as_ptr(), text.as_ptr());
                }
            }
        }
    }

    /// One tracing event: buffered, then sent as it is dropped.
    struct Event(Vec<u8>);

    impl Write for Event {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Event {
        fn drop(&mut self) {
            write(INFO, &String::from_utf8_lossy(&self.0));
        }
    }

    pub fn install(filter: tracing_subscriber::EnvFilter) {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .without_time()
            .with_writer(|| Event(Vec::new()))
            .init();
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            write(FATAL, &format!("panic: {info}"));
            default(info);
        }));
    }
}
