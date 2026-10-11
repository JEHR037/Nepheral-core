// SPDX-License-Identifier: BUSL-1.1
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Empty by default: the model to install is the one this machine's
/// hardware tier calls for (`ModelTier::model_url`). A non-empty value
/// here pins a specific model instead.
const DEFAULT_DOWNLOAD_URL: &str = "";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum BackendChoice {
    Local,
    #[default]
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NepheralConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    #[serde(default)]
    pub backend: BackendChoice,

    #[serde(default = "default_model_path")]
    pub model_path: String,

    #[serde(default)]
    pub external_url: Option<String>,

    #[serde(default = "default_model_tier")]
    pub model_tier: String,

    /// File name inside `~/.nepheral/models/` (or an absolute path) of the
    /// GGUF this node should load. Empty/absent = pick whatever is on disk.
    #[serde(default)]
    pub active_model: Option<String>,

    #[serde(default = "default_true")]
    pub local_api_server_enabled: bool,

    #[serde(default = "default_api_port")]
    pub local_api_server_port: u16,

    #[serde(default = "default_api_host")]
    pub local_api_server_host: String,

    #[serde(default)]
    pub local_api_server_token: Option<String>,

    /// Exact origins allowed to call the local API from a browser (e.g.
    /// "http://127.0.0.1:8080" for a page served locally). Empty
    /// (default) emits no CORS headers: cross-origin pages are blocked.
    #[serde(default)]
    pub local_api_server_allowed_origins: Vec<String>,

    #[serde(default = "default_download_url")]
    pub model_download_url: String,

    #[serde(default)]
    pub inference_tuning: Option<InferenceTuningConfig>,

    #[serde(default)]
    pub local_system_prompt: Option<String>,

    /// The public address of the wallet this node works for, so balances
    /// and the delegation can be shown while the wallet is locked.
    #[serde(default)]
    pub wallet_address: Option<String>,

    /// SHA-256 of the local API's token (see `wallet::api_token_hash`). The
    /// API checks requests against it, locked or not; the token itself is
    /// kept only in the wallet's vault.
    #[serde(default)]
    pub local_api_server_token_hash: Option<String>,

    /// Minutes of inactivity before the desktop's wallet locks itself; 0 is
    /// never, unset is `wallet::DEFAULT_AUTO_LOCK_MINUTES`.
    #[serde(default)]
    pub auto_lock_minutes: Option<u64>,

    #[serde(default = "default_true")]
    pub thinking_enabled: bool,

    #[serde(default)]
    pub api_routing_policy: Option<serde_json::Value>,

    /// True once the desktop's setup wizard has finished. The file itself
    /// cannot say this: creating the wallet writes it before the wizard
    /// opens. A file without the field reads as done: it came from an
    /// install that predates the flag, which either finished the wizard or
    /// (0.5.0-beta, which never showed it) picks models in the AI tab.
    #[serde(default = "default_true")]
    pub desktop_setup_done: bool,

    // ---- headless nepheral-node (server) settings, set by the CLI wizard ----
    /// True once the interactive first-run wizard has completed, so a
    /// systemd restart (no TTY attached) never re-prompts.
    #[serde(default)]
    pub node_setup_done: bool,
    #[serde(default = "default_node_port")]
    pub node_port: u16,
    /// Serve as a bootstrap/relay: other nodes dial this one to join the
    /// mesh and receive gossip-propagated peer lists.
    #[serde(default)]
    pub node_is_bootstrap: bool,
    /// "ip:port" entries this node dials on startup, in addition to
    /// whatever dials in — lets a relay stitch itself into an existing mesh.
    #[serde(default)]
    pub node_bootstrap_peers: Vec<String>,

    #[serde(default = "default_network")]
    pub network: String,

    #[serde(default = "default_mesh_port")]
    pub mesh_port: u16,

    #[serde(default)]
    pub redact_terms: Vec<String>,

    /// Keep conversations on this device between runs, sealed with the
    /// wallet's key (`conversations.rs`). Off: they live only in memory.
    #[serde(default = "default_true")]
    pub keep_conversations: bool,

    /// Until when (Unix ms) a question goes without asking first, whatever
    /// older context it carries: the person chose "don't ask for a while" on
    /// the confirmation card. `None`, or a time past, asks as usual.
    #[serde(default)]
    pub confirm_snooze_until_ms: Option<u64>,

    /// With several GPUs: "split" (one model across them, as llama.cpp does
    /// by itself) or "per_gpu" (a copy on each card, answering as many
    /// questions at once).
    #[serde(default = "default_gpu_mode")]
    pub gpu_mode: String,
}

fn default_gpu_mode() -> String {
    "split".to_string()
}

impl NepheralConfig {
    /// Whether answers come from an external backend the user configured.
    /// `backend` alone cannot say so: it defaults to External in a config
    /// nobody has chosen anything in yet.
    pub fn uses_external_backend(&self) -> bool {
        matches!(self.backend, BackendChoice::External)
            && self
                .external_url
                .as_deref()
                .is_some_and(|u| !u.trim().is_empty())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceTuningConfig {
    pub n_ctx: u32,
    pub n_ubatch: u32,
    pub max_new_tokens: u32,
    pub estimated_ram_mb: u64,
}

fn default_schema_version() -> u32 {
    1
}
fn default_download_url() -> String {
    DEFAULT_DOWNLOAD_URL.to_string()
}
fn default_true() -> bool {
    true
}
fn default_api_port() -> u16 {
    11434
}
fn default_api_host() -> String {
    "127.0.0.1".to_string()
}
fn default_model_tier() -> String {
    "standard".to_string()
}
fn default_node_port() -> u16 {
    443
}
fn default_network() -> String {
    core_crypto::network::Network::Testnet.name().to_string()
}
fn default_mesh_port() -> u16 {
    DEFAULT_MESH_PORT
}

pub const DEFAULT_MESH_PORT: u16 = 443;
pub const FALLBACK_MESH_PORT: u16 = 4433;

fn default_model_path() -> String {
    nepheral_dir()
        .join("model.gguf")
        .to_string_lossy()
        .to_string()
}

impl Default for NepheralConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            backend: BackendChoice::External,
            model_path: default_model_path(),
            external_url: None,
            model_tier: "standard".to_string(),
            active_model: None,
            local_api_server_enabled: true,
            local_api_server_port: 11434,
            local_api_server_host: "127.0.0.1".to_string(),
            local_api_server_token: None,
            local_api_server_allowed_origins: Vec::new(),
            model_download_url: DEFAULT_DOWNLOAD_URL.to_string(),
            inference_tuning: None,
            local_system_prompt: None,
            wallet_address: None,
            local_api_server_token_hash: None,
            auto_lock_minutes: None,
            thinking_enabled: true,
            api_routing_policy: None,
            desktop_setup_done: false,
            node_setup_done: false,
            node_port: default_node_port(),
            node_is_bootstrap: false,
            node_bootstrap_peers: Vec::new(),
            network: default_network(),
            mesh_port: DEFAULT_MESH_PORT,
            redact_terms: Vec::new(),
            keep_conversations: true,
            confirm_snooze_until_ms: None,
            gpu_mode: default_gpu_mode(),
        }
    }
}

pub fn configured_network() -> core_crypto::network::Network {
    if let Some(n) = std::env::var("NEPHERAL_NETWORK")
        .ok()
        .and_then(|v| core_crypto::network::Network::parse(&v))
    {
        return n;
    }
    load_config()
        .and_then(|c| core_crypto::network::Network::parse(&c.network))
        .unwrap_or(core_crypto::network::Network::Testnet)
}

pub fn network_dir(network: core_crypto::network::Network) -> PathBuf {
    nepheral_dir().join(network.data_subdir())
}

pub fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod {}: {e}", dir.display()))?;
    }
    Ok(())
}

pub fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    f.write_all(contents)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("sync {}: {e}", path.display()))
}

pub fn nepheral_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = test_support::dir_override() {
        return dir;
    }
    // The phone app points this at its own data directory: a phone has no
    // home directory to put .nepheral in.
    if let Some(dir) = std::env::var_os("NEPHERAL_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    Path::new(&home).join(".nepheral")
}

/// Redirects the configuration directory for one test thread.
///
/// Thread-local rather than an environment variable so tests running in
/// parallel cannot see each other's directory, and so nothing outside a test
/// build can reach it.
#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    pub fn dir_override() -> Option<PathBuf> {
        DIR.with(|d| d.borrow().clone())
    }

    /// A private directory for this test, removed when the guard drops.
    pub struct TempHome(PathBuf);

    impl TempHome {
        pub fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "nepheral-config-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp home");
            DIR.with(|d| *d.borrow_mut() = Some(dir.clone()));
            Self(dir)
        }

        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            DIR.with(|d| *d.borrow_mut() = None);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

pub fn config_path() -> PathBuf {
    nepheral_dir().join("config.json")
}

pub fn load_config() -> Option<NepheralConfig> {
    let path = config_path();
    if !path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes the whole configuration.
///
/// Prefer [`update`] for changing individual settings: this replaces every
/// field, so a caller holding a stale copy reverts anything saved since it
/// was loaded.
pub fn save_config(config: &NepheralConfig) -> Result<(), String> {
    let dir = nepheral_dir();
    ensure_private_dir(&dir)?;
    let text =
        serde_json::to_string_pretty(config).map_err(|e| format!("serialize config: {e}"))?;

    // Written beside the target and renamed over it. A partial write leaves a
    // config.json that does not parse, and `load_config` answers that with
    // `None` — which reads as "first run" and quietly discards the user's
    // settings, including which model they chose.
    let final_path = config_path();
    let tmp_path = final_path.with_extension("json.tmp");
    write_private_file(&tmp_path, text.as_bytes())?;
    std::fs::rename(&tmp_path, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!("replace config.json: {e}")
    })?;
    Ok(())
}

/// Changes settings on top of whatever is on disk right now.
///
/// Every caller owns a handful of fields, but the file is written whole. A
/// caller that serialises a copy loaded at startup therefore reverts each
/// field another path has written since — which is how a model chosen in one
/// screen was undone by pressing Apply in another. Re-reading immediately
/// before the write keeps each caller to its own fields.
///
/// Returns the configuration as saved, so a caller keeping a cached copy can
/// replace it with one that is current.
pub fn update<F>(mutate: F) -> Result<NepheralConfig, String>
where
    F: FnOnce(&mut NepheralConfig),
{
    let mut config = load_config().unwrap_or_default();
    mutate(&mut config);
    save_config(&config)?;
    Ok(config)
}

/// Load the persisted 24-word identity from `~/.nepheral/identity.mnemonic`,
/// generating and saving one on first run. Shared by every binary so a
/// node's peer ID survives restarts unless `NEPHERAL_NODE_MNEMONIC`
/// overrides it.
pub fn load_or_create_identity_mnemonic() -> Result<core_crypto::NodeMnemonic, String> {
    let dir = nepheral_dir();
    let path = dir.join("identity.mnemonic");

    if let Ok(phrase) = std::env::var("NEPHERAL_NODE_MNEMONIC") {
        return core_crypto::NodeMnemonic::from_phrase(phrase.trim());
    }

    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let phrase = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read identity file: {e}"))?;
        return core_crypto::NodeMnemonic::from_phrase(phrase.trim());
    }

    let m = core_crypto::NodeMnemonic::generate();
    ensure_private_dir(&dir)?;
    write_private_file(&path, m.phrase().as_bytes())?;
    tracing::info!(
        path = %path.display(),
        "New wallet generated — back up its recovery phrase from the app's settings, it is never written to the log"
    );
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::test_support::TempHome;
    use super::*;

    #[test]
    fn an_update_keeps_settings_it_did_not_touch() {
        // The regression this exists for: choosing a model wrote
        // `active_model`, and the next unrelated save — built from a copy of
        // the file loaded when the window opened — put it back to `None`. The
        // app then fell back to whichever file sorted first on disk.
        let _home = TempHome::new("keeps");

        update(|cfg| cfg.active_model = Some("chosen.gguf".to_string())).expect("record model");
        let stale = load_config().expect("written");

        update(|cfg| cfg.thinking_enabled = !cfg.thinking_enabled).expect("unrelated setting");

        let after = load_config().expect("still there");
        assert_eq!(
            after.active_model.as_deref(),
            Some("chosen.gguf"),
            "an unrelated setting must not revert the chosen model"
        );
        assert_ne!(after.thinking_enabled, stale.thinking_enabled);
    }

    #[test]
    fn a_whole_file_write_from_a_stale_copy_is_what_reverts_things() {
        // Kept as the counter-example: this is the shape `update` replaces,
        // and it documents why calling `save_config` with a held copy is not
        // an equivalent way to store one field.
        let _home = TempHome::new("stale");

        let stale = NepheralConfig::default();
        update(|cfg| cfg.active_model = Some("chosen.gguf".to_string())).expect("record model");

        save_config(&stale).expect("whole-file write");

        assert_eq!(
            load_config().and_then(|c| c.active_model),
            None,
            "a whole-file write from an old copy discards newer fields"
        );
    }

    #[test]
    fn an_update_on_a_machine_with_no_config_yet_starts_from_defaults() {
        let _home = TempHome::new("firstrun");
        assert!(load_config().is_none());

        let saved = update(|cfg| cfg.active_model = Some("first.gguf".to_string()))
            .expect("first write creates the file");

        assert_eq!(saved.active_model.as_deref(), Some("first.gguf"));
        assert_eq!(saved.schema_version, default_schema_version());
        assert_eq!(
            load_config().and_then(|c| c.active_model).as_deref(),
            Some("first.gguf")
        );
    }

    #[test]
    fn a_save_leaves_no_temporary_file_behind() {
        // The temporary is renamed over the target rather than left beside
        // it, so a later run cannot mistake a half-written file for the
        // configuration.
        let _home = TempHome::new("atomic");
        update(|cfg| cfg.active_model = Some("m.gguf".to_string())).expect("write");

        let leftovers: Vec<_> = std::fs::read_dir(nepheral_dir())
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_identity_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let _home = TempHome::new("identity");
        load_or_create_identity_mnemonic().expect("generate");
        let mode = std::fs::metadata(nepheral_dir().join("identity.mnemonic"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "identity readable by others: {mode:o}");
        let cfg_mode = {
            update(|c| c.thinking_enabled = false).unwrap();
            std::fs::metadata(config_path())
                .unwrap()
                .permissions()
                .mode()
        };
        assert_eq!(
            cfg_mode & 0o077,
            0,
            "config readable by others: {cfg_mode:o}"
        );
    }

    #[test]
    fn an_unreadable_config_is_not_silently_adopted_as_a_fresh_one() {
        // `load_config` answers a corrupt file with `None`, which `update`
        // turns into defaults. That is recoverable only because the write
        // itself is atomic: the file cannot be left half-written in the first
        // place. This pins the behaviour so the two stay in step.
        let _home = TempHome::new("corrupt");
        std::fs::write(config_path(), b"{ this is not json").expect("write garbage");
        assert!(load_config().is_none());

        let saved =
            update(|cfg| cfg.active_model = Some("recovered.gguf".to_string())).expect("recover");
        assert_eq!(saved.active_model.as_deref(), Some("recovered.gguf"));
    }
}
