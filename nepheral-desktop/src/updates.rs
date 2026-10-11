// SPDX-License-Identifier: BUSL-1.1
//! Desktop updates: Tauri's updater against the download depot.
//!
//! Each build checks the feed of its own variant, so a CPU build stays a CPU
//! build and a CUDA one keeps its CUDA libraries. The feed is a static JSON
//! file on `dl.nepheral.net`; every installer it points to is signed with
//! the release key, whose public half is `plugins.updater.pubkey` in
//! `tauri.conf.json`, and the updater refuses one whose signature does not
//! verify.
//!
//! Phones update through their own store or the APK feed, not this.

use serde::Serialize;
#[cfg(desktop)]
use tauri_plugin_updater::UpdaterExt;

/// Which build this is.
#[cfg(desktop)]
pub const VARIANT: &str = if cfg!(feature = "gpu-auto") || cfg!(feature = "cuda") {
    "cuda"
} else if cfg!(feature = "vulkan") {
    "vulkan"
} else {
    "cpu"
};

#[cfg(desktop)]
pub fn feed_url() -> String {
    format!("https://dl.nepheral.net/updates/desktop-{VARIANT}.json")
}

#[derive(Serialize)]
pub struct UpdateView {
    pub version: String,
    pub current: String,
    pub notes: Option<String>,
}

#[cfg(desktop)]
fn updater(app: &tauri::AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let url = feed_url()
        .parse()
        .map_err(|e| format!("bad update feed address: {e}"))?;
    app.updater_builder()
        .endpoints(vec![url])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())
}

/// A newer release of this variant, if there is one.
#[cfg(desktop)]
#[tauri::command]
pub async fn update_check(app: tauri::AppHandle) -> Result<Option<UpdateView>, String> {
    if crate::is_light() {
        return Ok(None);
    }
    let update = updater(&app)?.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateView {
        version: u.version.clone(),
        current: u.current_version.clone(),
        notes: u.body.clone(),
    }))
}

/// Downloads the newer release, checks its signature, installs it and
/// restarts into it. The node is shut down first, so the chain file and the
/// wallet are written out before the installer replaces the program.
#[cfg(desktop)]
#[tauri::command]
pub async fn update_install(app: tauri::AppHandle) -> Result<(), String> {
    let Some(update) = updater(&app)?.check().await.map_err(|e| e.to_string())? else {
        return Err("no update available".into());
    };
    let confirmed = {
        let app = app.clone();
        let version = update.version.clone();
        tokio::task::spawn_blocking(move || confirm(&app, &version))
            .await
            .unwrap_or(false)
    };
    if !confirmed {
        return Err("declined: update".into());
    }
    tracing::info!(version = %update.version, "Downloading update");
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if let Some(node) = crate::gate::NODE.get().copied() {
        tracing::info!("Stopping the node before installing the update");
        // shutdown() blocks on the node's own runtime, which cannot happen
        // on a thread already driving this command's.
        let _ = tokio::task::spawn_blocking(move || node.shutdown()).await;
    }
    if let Err(e) = update.install(bytes) {
        // The node is already down: restart into the old version rather
        // than leave a window with nothing behind it.
        tracing::error!(error = %e, "Update install failed; restarting the current version");
    }
    // On Windows the installer has taken over and restarts the app itself;
    // elsewhere the new files are in place and this restarts into them.
    app.restart();
}

/// Asks before updating, in the window's language: the app closes, installs
/// and opens again, which interrupts whatever the node was doing.
#[cfg(desktop)]
fn confirm(app: &tauri::AppHandle, version: &str) -> bool {
    use tauri::Manager;
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let es = app
        .try_state::<crate::commands::Desktop>()
        .and_then(|d| d.ui_lang.lock().ok().map(|l| *l == "es"))
        .unwrap_or(false);
    let (title, body, yes, no) = if es {
        (
            "Actualizar Nepheral",
            format!(
                "Se descargará e instalará la versión {version}. Nepheral se cerrará y \
                 volverá a abrirse solo. Tu cartera y tus datos no cambian."
            ),
            "Actualizar",
            "Ahora no",
        )
    } else {
        (
            "Update Nepheral",
            format!(
                "Version {version} will be downloaded and installed. Nepheral closes and \
                 opens again by itself. Your wallet and data stay as they are."
            ),
            "Update",
            "Not now",
        )
    };
    let mut dialog = app
        .dialog()
        .message(body)
        .title(title)
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(yes.into(), no.into()));
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.parent(&w);
    }
    dialog.blocking_show()
}

/// The phone: installed from Google Play, it updates through Play only.
/// Installed from the website's APK, it reads the website's manifest, and a
/// newer version is offered for download.
#[cfg(mobile)]
const MANIFESTS: &[&str] = &[
    "https://dl.nepheral.net/releases/manifest.json",
    "https://dl.93.189.89.201.nip.io/releases/manifest.json",
];

#[cfg(mobile)]
async fn newer_apk() -> Result<Option<(String, String)>, String> {
    #[cfg(target_os = "android")]
    if crate::android::installed_from_play() {
        return Ok(None);
    }
    let client = ai_engine::http_client()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let mut last = String::new();
    for url in MANIFESTS {
        let manifest: serde_json::Value = match client.get(*url).send().await {
            Ok(r) => match r.json().await {
                Ok(v) => v,
                Err(e) => {
                    last = e.to_string();
                    continue;
                }
            },
            Err(e) => {
                last = e.to_string();
                continue;
            }
        };
        let version = manifest["version"].as_str().unwrap_or_default().to_string();
        let file = manifest["builds"].as_array().and_then(|b| {
            b.iter()
                .find(|x| x["os"] == "android" && x["kind"] == "mobile")
                .and_then(|x| x["file"].as_str())
                .map(str::to_string)
        });
        let Some(file) = file else {
            return Ok(None);
        };
        if !is_newer(&version, env!("CARGO_PKG_VERSION")) {
            return Ok(None);
        }
        let base = url.trim_end_matches("releases/manifest.json");
        return Ok(Some((version, format!("{base}{file}"))));
    }
    Err(last)
}

/// "0.5.4-beta" over "0.5.3": the numbers decide, a suffix does not.
#[cfg_attr(not(mobile), allow(dead_code))]
fn is_newer(candidate: &str, current: &str) -> bool {
    let nums = |v: &str| -> Vec<u64> {
        v.split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    nums(candidate) > nums(current)
}

#[cfg(mobile)]
#[tauri::command]
pub async fn update_check() -> Result<Option<UpdateView>, String> {
    Ok(newer_apk().await?.map(|(version, _)| UpdateView {
        version,
        current: env!("CARGO_PKG_VERSION").to_string(),
        notes: None,
    }))
}

/// Opens the newer APK's download in the browser; Android then installs it
/// over this copy only if it is signed with the same key.
#[cfg(mobile)]
#[tauri::command]
pub async fn update_install() -> Result<(), String> {
    let Some((_, url)) = newer_apk().await? else {
        return Err("no update available".into());
    };
    #[cfg(target_os = "android")]
    {
        crate::android::open_url(&url)
    }
    #[cfg(not(target_os = "android"))]
    {
        Err(format!("download it from {url}"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_compare_by_their_numbers() {
        assert!(super::is_newer("0.5.4-beta", "0.5.3"));
        assert!(super::is_newer("0.6.0-beta", "0.5.9"));
        assert!(!super::is_newer("0.5.3-beta", "0.5.3"));
        assert!(!super::is_newer("0.5.2-beta", "0.5.3"));
    }
}
