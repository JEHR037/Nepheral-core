// SPDX-License-Identifier: BUSL-1.1
// Hide the Windows console in release builds. Nothing is logged to disk
// unless NEPHERAL_LOG_FILE asks for it (see init_logging in lib.rs).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    nepheral_desktop::run(tauri::generate_context!(), nepheral_desktop::Profile::Full);
}
