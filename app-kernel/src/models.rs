// SPDX-License-Identifier: BUSL-1.1

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use ai_engine::InferenceEngine;
use core_crypto::identity::PeerId;
use mesh_network::protocol::{frame, message_types};
use tracing::info;

use crate::state::{send_frame, AppState};

pub const MAX_MODEL_TRANSFER_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_CONCURRENT_UPLOADS: usize = 2;

enum GgufHealth {
    Ok,
    Corrupt,
    Unreadable,
}

/// The name a node advertises for a model loaded from `path`.
///
/// It travels in HELLO, is what every other node shows for this one, and is
/// what `params_b_from_name` reads the size from - so it sets what each answer
/// earns. Defaulting it to "local" made a whole mesh of distinct models look
/// identical and all earn as 1B; the GGUF's own file name says what it is.
pub fn model_name_for(path: &str, explicit: Option<String>) -> String {
    explicit
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| {
            std::path::Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "local".into())
        })
}

/// The model's size, read from its name: 1 when the name says nothing.
pub fn params_b_from_name(name: &str) -> u16 {
    size_in_name(name).unwrap_or(1)
}

/// A size such as "9B" or "1.5b" in a model's name, rounded up; the
/// largest when there are several.
pub fn size_in_name(name: &str) -> Option<u16> {
    let lower = name.to_lowercase();
    let bytes = lower.as_bytes();
    let mut best: Option<f32> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'b' {
                let after = i + 1;
                if after >= bytes.len() || !bytes[after].is_ascii_alphanumeric() {
                    if let Ok(v) = lower[start..i].parse::<f32>() {
                        best = Some(best.map_or(v, |b: f32| b.max(v)));
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    best.map(|v| v.ceil().clamp(1.0, u16::MAX as f32) as u16)
}

pub fn tier_for_params(params_b: u16) -> u8 {
    match params_b {
        0..=2 => 0,
        3..=5 => 1,
        6..=9 => 2,
        10..=20 => 3,
        21..=40 => 4,
        _ => 5,
    }
}

impl AppState {
    pub fn models_dir() -> PathBuf {
        ai_engine::ModelStore::default_dir().join("models")
    }

    pub fn local_model_path() -> Option<PathBuf> {
        let store = ai_engine::ModelStore::default_dir();
        ai_engine::ModelStore::new(store.clone()).migrate_legacy_model();
        let models_dir = Self::models_dir();
        let mut candidates: Vec<(PathBuf, bool)> = Vec::new();

        if let Ok(p) = std::env::var("NEPHERAL_MODEL_PATH") {
            candidates.push((PathBuf::from(p), false));
        }
        if let Some(cfg) = crate::nepheral_config::load_config() {
            if let Some(active) = cfg.active_model.filter(|s| !s.trim().is_empty()) {
                if let Some(name) = ai_engine::model_store::local_file_name(&active) {
                    candidates.push((models_dir.join(name), true));
                }
            }
        }
        if let Ok(entries) = std::fs::read_dir(&models_dir) {
            let mut found: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .map(|x| x.eq_ignore_ascii_case("gguf"))
                        .unwrap_or(false)
                })
                .collect();
            found.sort();
            candidates.extend(found.into_iter().map(|p| (p, true)));
        }

        for (path, managed) in candidates {
            if !path.exists() {
                continue;
            }
            match Self::gguf_health(&path) {
                GgufHealth::Ok => return Some(path),
                GgufHealth::Corrupt if managed => {
                    tracing::warn!(path = %path.display(), "Not a valid GGUF — removing so it can be fetched again");
                    let _ = std::fs::remove_file(&path);
                }
                GgufHealth::Corrupt => {
                    tracing::warn!(path = %path.display(), "Configured model path is not a valid GGUF — skipping");
                }
                GgufHealth::Unreadable => {
                    tracing::warn!(path = %path.display(), "Model file unreadable — skipping");
                }
            }
        }
        None
    }

    fn gguf_health(path: &Path) -> GgufHealth {
        const SANITY_FLOOR_BYTES: u64 = 20 * 1024 * 1024;
        let magic = std::fs::File::open(path).ok().and_then(|mut f| {
            use std::io::Read;
            let mut m = [0u8; 4];
            f.read_exact(&mut m).ok().map(|_| m)
        });
        match magic {
            Some([0x47, 0x47, 0x55, 0x46]) => {
                let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                if size < SANITY_FLOOR_BYTES {
                    GgufHealth::Corrupt
                } else {
                    GgufHealth::Ok
                }
            }
            Some(_) => GgufHealth::Corrupt,
            None => GgufHealth::Unreadable,
        }
    }

    pub async fn cached_model_hash(&self, path: &Path) -> Option<(u64, [u8; 32])> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncReadExt;
        let size = tokio::fs::metadata(path).await.ok()?.len();
        if let Some((cached_size, hash)) = *self.model_file_hash.read().await {
            if cached_size == size {
                return Some((size, hash));
            }
        }
        let mut f = tokio::fs::File::open(path).await.ok()?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = f.read(&mut buf).await.ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let hash: [u8; 32] = hasher.finalize().into();
        *self.model_file_hash.write().await = Some((size, hash));
        Some((size, hash))
    }

    pub async fn announce_model_to_peer(&self, conn: &quinn::Connection) {
        let Some(path) = Self::local_model_path() else {
            return;
        };
        let Some((file_size, hash)) = self.cached_model_hash(&path).await else {
            return;
        };
        let mut payload = Vec::with_capacity(40);
        payload.extend_from_slice(&hash);
        payload.extend_from_slice(&file_size.to_be_bytes());
        let _ = send_frame(conn, &frame(message_types::MODEL_ANNOUNCE, &payload)).await;
    }

    pub async fn on_model_announce(&self, from: PeerId, hash: [u8; 32], size: u64) {
        if !(ai_engine::MIN_PLAUSIBLE_MODEL_BYTES..=MAX_MODEL_TRANSFER_BYTES).contains(&size) {
            return;
        }
        self.peers_with_model
            .write()
            .await
            .insert(from, (hash, size));
    }

    pub async fn serve_model_to_peer(
        &self,
        remote_addr: SocketAddr,
        requester: PeerId,
        requested: [u8; 32],
    ) {
        let Some(path) = Self::local_model_path() else {
            return;
        };
        let Some((file_size, hash)) = self.cached_model_hash(&path).await else {
            return;
        };
        if hash != requested {
            tracing::info!(peer = %remote_addr, "MODEL_REQUEST for a model this node does not hold");
            return;
        }
        if self.active_model_uploads.fetch_add(1, Ordering::SeqCst) >= MAX_CONCURRENT_UPLOADS {
            self.active_model_uploads.fetch_sub(1, Ordering::SeqCst);
            tracing::info!(peer = %remote_addr, "MODEL_REQUEST declined — upload slots busy");
            return;
        }
        let Some((_, conn)) = self.connection_for_peer(&requester).await else {
            self.active_model_uploads.fetch_sub(1, Ordering::SeqCst);
            return;
        };
        let uploads = self.active_model_uploads.clone();
        tokio::spawn(async move {
            let result: Result<(), String> = async {
                let (mut send, _) = tokio::time::timeout(Duration::from_secs(10), conn.open_bi())
                    .await
                    .map_err(|_| "open timeout".to_string())?
                    .map_err(|e| e.to_string())?;
                send.write_all(&[message_types::MODEL_TRANSFER])
                    .await
                    .map_err(|e| e.to_string())?;
                send.write_all(&hash).await.map_err(|e| e.to_string())?;
                send.write_all(&file_size.to_be_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                let mut file = tokio::fs::File::open(&path)
                    .await
                    .map_err(|e| e.to_string())?;
                tokio::io::copy(&mut file, &mut send)
                    .await
                    .map_err(|e| e.to_string())?;
                send.finish().map_err(|e| e.to_string())?;
                let _ = send.stopped().await;
                Ok(())
            }
            .await;
            uploads.fetch_sub(1, Ordering::SeqCst);
            match result {
                Ok(()) => {
                    tracing::info!(peer = %remote_addr, bytes = file_size, "Model served to peer")
                }
                Err(e) => tracing::warn!(peer = %remote_addr, error = %e, "Model upload failed"),
            }
        });
    }

    pub async fn request_model_from_peer(
        &self,
        peer: PeerId,
        trusted_hash: [u8; 32],
        trusted_size: u64,
        file_name: String,
    ) -> Result<(), String> {
        let announced = self.peers_with_model.read().await.get(&peer).copied();
        match announced {
            Some((hash, size)) if hash == trusted_hash && size == trusted_size => {}
            Some(_) => return Err("that peer holds a different model than the verified one".into()),
            None => return Err("that peer has not announced a model".into()),
        }
        let Some((_, conn)) = self.connection_for_peer(&peer).await else {
            return Err("no live connection to that peer".into());
        };
        self.pending_model_requests
            .lock()
            .await
            .insert(peer, ((trusted_hash, trusted_size), file_name));
        send_frame(&conn, &frame(message_types::MODEL_REQUEST, &trusted_hash)).await
    }

    pub async fn receive_model_transfer(
        &self,
        from: PeerId,
        hash: [u8; 32],
        total_size: u64,
        mut recv: quinn::RecvStream,
    ) {
        let expected = self.pending_model_requests.lock().await.remove(&from);
        let target_name = match expected {
            Some(((h, s), name)) if h == hash && s == total_size => name,
            _ => {
                tracing::warn!(
                    from = %hex::encode(&from[..8]),
                    "Unrequested or mismatched MODEL_TRANSFER refused"
                );
                let _ = recv.stop(0u32.into());
                return;
            }
        };
        let Some(file_name) = ai_engine::model_store::local_file_name(&target_name) else {
            return;
        };
        let dir = Self::models_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let part_path = dir.join(format!("{file_name}.{}.part", hex::encode(&hash[..6])));
        let final_path = dir.join(&file_name);
        *self.model_download_progress.lock().await = Some((0, total_size));

        use sha2::{Digest, Sha256};
        use tokio::io::AsyncWriteExt;
        let outcome: Result<(), String> = async {
            let mut file = tokio::fs::File::create(&part_path)
                .await
                .map_err(|e| format!("cannot create part file: {e}"))?;
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 512 * 1024];
            let mut written: u64 = 0;
            loop {
                let chunk = tokio::time::timeout(Duration::from_secs(60), recv.read(&mut buf))
                    .await
                    .map_err(|_| "transfer stalled".to_string())?
                    .map_err(|e| e.to_string())?;
                let Some(n) = chunk else { break };
                if written + n as u64 > total_size {
                    return Err("peer sent more than announced".to_string());
                }
                file.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
                hasher.update(&buf[..n]);
                written += n as u64;
                *self.model_download_progress.lock().await = Some((written, total_size));
            }
            file.sync_all().await.map_err(|e| e.to_string())?;
            if written != total_size {
                return Err(format!("received {written} of {total_size} bytes"));
            }
            let got: [u8; 32] = hasher.finalize().into();
            if got != hash {
                return Err("SHA-256 does not match the verified hash".to_string());
            }
            tokio::fs::rename(&part_path, &final_path)
                .await
                .map_err(|e| format!("rename failed: {e}"))?;
            Ok(())
        }
        .await;
        *self.model_download_progress.lock().await = None;
        match outcome {
            Ok(()) => {
                info!(path = %final_path.display(), "Verified model received from peer");
                *self.model_download_error.lock().await = None;
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part_path).await;
                tracing::warn!(error = %e, "Model transfer rejected");
                *self.model_download_error.lock().await = Some(e);
            }
        }
        self.model_transfer_notify.notify_waiters();
    }

    pub async fn trusted_model_target(&self) -> Result<(String, String, [u8; 32], u64), String> {
        let url = match crate::nepheral_config::load_config()
            .map(|c| c.model_download_url)
            .filter(|u| !u.trim().is_empty())
        {
            Some(u) => u,
            None => ai_engine::hw_detect::detect()
                .recommended_tier
                .model_url()
                .ok_or_else(|| "this hardware cannot run a local model".to_string())?,
        };
        let (repo, file) = ai_engine::model_store::parse_hf_url(&url).ok_or_else(|| {
            format!("{url} is not a huggingface.co resolve link, so no publisher checksum exists")
        })?;
        let files = ai_engine::model_catalog::list_files(&repo)
            .await
            .map_err(|e| format!("could not read the publisher's checksums: {e}"))?;
        let entry = files
            .into_iter()
            .find(|f| f.file == file)
            .ok_or_else(|| format!("{repo} does not list {file}"))?;
        let hash = entry
            .sha256_hex
            .as_deref()
            .and_then(ai_engine::model_store::decode_sha256_hex)
            .ok_or_else(|| format!("{repo} publishes no checksum for {file}"))?;
        Ok((url, file, hash, entry.size_bytes))
    }

    pub async fn request_trusted_model_from_peer(&self, peer: PeerId) -> Result<(), String> {
        let (_, file, hash, size) = self.trusted_model_target().await?;
        self.pending_model_requests.lock().await.remove(&peer);
        self.request_model_from_peer(peer, hash, size, file).await
    }

    pub async fn run_model_sync(self) {
        use ai_engine::model_store::ModelStore;
        use std::sync::atomic::AtomicU64;

        if let Some(path) = Self::local_model_path() {
            self.adopt_model_path(&path).await;
            return;
        }

        let (url, file, hash, size) = match self.trusted_model_target().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "No verified model target — staying without a local model");
                *self.model_download_error.lock().await = Some(e);
                return;
            }
        };

        const P2P_WAIT: Duration = Duration::from_secs(30);
        let deadline = std::time::Instant::now() + P2P_WAIT;
        let peer = loop {
            if std::time::Instant::now() >= deadline || !self.running().await {
                break None;
            }
            let candidate = self
                .peers_with_model
                .read()
                .await
                .iter()
                .find(|(_, (h, s))| *h == hash && *s == size)
                .map(|(p, _)| *p);
            if candidate.is_some() {
                break candidate;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        };

        if let Some(peer) = peer {
            let waiter = self.model_transfer_notify.clone();
            let notified = waiter.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .request_model_from_peer(peer, hash, size, file.clone())
                .await
                .is_ok()
            {
                let _ = tokio::time::timeout(Duration::from_secs(30 * 60), notified).await;
                let target = Self::models_dir().join(&file);
                if target.exists() {
                    let _ =
                        crate::nepheral_config::update(|cfg| cfg.active_model = Some(file.clone()));
                    self.adopt_model_path(&target).await;
                    return;
                }
            }
        }

        *self.model_download_error.lock().await = None;
        let store = ModelStore::new(ModelStore::default_dir());
        let last_pct = Arc::new(AtomicU64::new(999));
        let progress_state = self.clone();
        let progress = move |done: u64, total: u64| {
            if let Ok(mut p) = progress_state.model_download_progress.try_lock() {
                *p = Some((done, total));
            }
            let pct = (done * 100).checked_div(total).unwrap_or(0);
            let prev = last_pct.load(Ordering::Relaxed);
            if prev == 999 || pct / 10 > prev / 10 {
                last_pct.store(pct, Ordering::Relaxed);
                tracing::info!(pct, "Model download progress");
            }
        };
        let hash_hex = hex::encode(hash);
        let result = store
            .download_model(&url, &file, Some(&hash_hex), progress)
            .await;
        *self.model_download_progress.lock().await = None;
        match result {
            Ok(path) => {
                let _ = crate::nepheral_config::update(|cfg| cfg.active_model = Some(file.clone()));
                self.adopt_model_path(&path).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "Model download failed");
                *self.model_download_error.lock().await = Some(e.to_string());
            }
        }
    }

    pub async fn adopt_model_path(&self, path: &Path) {
        {
            let mut cfg = self.ai_model_config.write().await;
            cfg.model_path = Some(path.to_string_lossy().to_string());
            cfg.model_name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "local".to_string());
        }
        let _ = self.cached_model_hash(path).await;
        match self.start_ai_engine().await {
            Ok(()) => {
                info!("Local AI engine ready");
                self.announce_capabilities().await;
            }
            Err(e) => tracing::warn!(error = %e, "Failed to initialize local AI engine"),
        }
    }

    pub async fn initialize_ai_engine(
        &self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let _init_guard = self.ai_engine_init_lock.lock().await;
        self.teardown_ai_engine().await;

        let mut config = self.ai_model_config.read().await.clone();
        if config.model_path.is_none() && config.backend_url.trim().is_empty() {
            if let Some(path) = Self::local_model_path() {
                let found = path.to_string_lossy().to_string();
                config.model_path = Some(found.clone());
                self.ai_model_config.write().await.model_path = Some(found);
            }
        }

        let stored = crate::nepheral_config::load_config().and_then(|c| c.inference_tuning);
        let tuning = match stored {
            Some(manual) => Some(manual),
            None => self.tuning_for_current_model().await,
        };
        if let Some(tuning) = tuning {
            config.context_window = tuning.n_ctx as usize;
            config.n_ubatch = tuning.n_ubatch;
            config.max_tokens = tuning.max_new_tokens as usize;
        }
        // The desktop's own model reasons when its owner turned that on.
        // Headless nodes answer the mesh and keep it off.
        #[cfg(feature = "client")]
        {
            config.allow_reasoning = crate::nepheral_config::load_config()
                .map(|c| c.thinking_enabled)
                .unwrap_or(false);
        }

        // Per-GPU mode: worker 0 on the first card, a copy of the model on
        // each other one.
        let per_gpu = std::env::var("NEPHERAL_GPU_MODE")
            .ok()
            .or_else(|| crate::nepheral_config::load_config().map(|c| c.gpu_mode))
            .is_some_and(|m| m.trim() == "per_gpu");
        let gpus = if per_gpu && config.model_path.is_some() {
            ai_engine::gpu_workers()
        } else {
            Vec::new()
        };
        if gpus.len() > 1 {
            config.devices = Some(vec![gpus[0]]);
            info!(gpus = ?gpus, "One model per GPU");
        }
        info!(model = %config.model_name, url = %config.backend_url, "Initializing AI engine");
        let engine = InferenceEngine::new(config.clone());
        if let Err(e) = engine.initialize().await {
            self.record_ai_engine_failure(&e.to_string()).await;
            return Err(e);
        }
        let mut extras = Vec::new();
        for gpu in gpus.iter().skip(1) {
            let mut c = config.clone();
            c.devices = Some(vec![*gpu]);
            let e = InferenceEngine::new(c);
            match e.initialize().await {
                Ok(()) => {
                    e.set_system_prompt(self.active_system_prompt().await).await;
                    extras.push(Arc::new(e));
                }
                Err(err) => tracing::warn!(gpu, error = %err, "A GPU's worker did not load"),
            }
        }
        *self.extra_busy.lock().await = vec![false; extras.len()];
        *self.extra_engines.write().await = extras;
        let arc_engine = Arc::new(engine);
        arc_engine
            .set_system_prompt(self.active_system_prompt().await)
            .await;
        *self.ai_engine.write().await = Some(arc_engine);
        if let Ok(mut cache) = self.ai_engine_status_cache.lock() {
            *cache = "ready".to_string();
        }
        if let Some(path) = self.ai_model_config.read().await.model_path.clone() {
            let _ = self.cached_model_hash(Path::new(&path)).await;
        }
        self.log("AI engine initialized").await;
        Ok(())
    }

    pub async fn start_ai_engine(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let first_error = match self.initialize_ai_engine().await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        let refused = self.ai_model_config.read().await.model_path.clone();
        let store = ai_engine::ModelStore::new(ai_engine::ModelStore::default_dir());
        let alternatives: Vec<_> = store
            .installed()
            .into_iter()
            .filter(|m| {
                refused
                    .as_deref()
                    .map(|r| Path::new(r) != m.path)
                    .unwrap_or(true)
            })
            .collect();
        for candidate in alternatives {
            {
                let mut cfg = self.ai_model_config.write().await;
                cfg.model_path = Some(candidate.path.to_string_lossy().to_string());
                cfg.model_name = candidate
                    .path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "local".to_string());
            }
            if self.initialize_ai_engine().await.is_ok() {
                tracing::warn!(
                    reason = %first_error,
                    now_using = %candidate.file_name,
                    "Configured model could not be loaded; another installed model is in use"
                );
                let chosen = candidate.file_name.clone();
                let _ = crate::nepheral_config::update(|cfg| cfg.active_model = Some(chosen));
                return Ok(());
            }
        }
        if let Some(path) = refused {
            self.ai_model_config.write().await.model_path = Some(path);
        }
        Err(first_error)
    }

    pub async fn refresh_system_prompt(&self) {
        let prompt = self.active_system_prompt().await;
        if let Some(engine) = self.ai_engine.read().await.as_ref() {
            engine.set_system_prompt(prompt.clone()).await;
        }
        for e in self.extra_engines.read().await.iter() {
            e.set_system_prompt(prompt.clone()).await;
        }
    }

    /// Models answering at once: one, or one per GPU in per-GPU mode.
    pub async fn worker_count(&self) -> usize {
        1 + self.extra_engines.read().await.len()
    }

    /// Every worker taken, by mesh claims or the owner's chats.
    pub async fn all_workers_busy(&self) -> bool {
        let workers = self.worker_count().await;
        let main = usize::from(*self.inference_busy.lock().await);
        let extra = self.extra_busy.lock().await.iter().filter(|b| **b).count();
        main + extra >= workers
    }

    /// A free worker, marked busy: 0 is the main engine.
    pub async fn take_worker(&self) -> Option<(usize, Arc<InferenceEngine>)> {
        if let Some(main) = self.ai_engine.read().await.clone() {
            let mut busy = self.inference_busy.lock().await;
            if !*busy {
                *busy = true;
                return Some((0, main));
            }
        }
        let extras = self.extra_engines.read().await.clone();
        let mut busy = self.extra_busy.lock().await;
        for (i, e) in extras.iter().enumerate() {
            if busy.get(i) == Some(&false) {
                busy[i] = true;
                return Some((i + 1, e.clone()));
            }
        }
        None
    }

    pub async fn free_worker(&self, worker: usize) {
        if worker == 0 {
            *self.inference_busy.lock().await = false;
        } else if let Some(b) = self.extra_busy.lock().await.get_mut(worker - 1) {
            *b = false;
        }
    }

    /// Applies the reasoning switch to the running engine, without a reload.
    pub async fn set_reasoning_allowed(&self, allowed: bool) {
        self.ai_model_config.write().await.allow_reasoning = allowed;
        if let Some(engine) = self.ai_engine.read().await.as_ref() {
            let mut config = engine.get_config().await;
            config.allow_reasoning = allowed;
            engine.set_config(config).await;
        }
    }

    pub async fn active_system_prompt(&self) -> Option<String> {
        if let Some(p) = self.local_system_prompt.read().await.clone() {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        if let Some(p) = crate::nepheral_config::load_config().and_then(|c| c.local_system_prompt) {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        std::env::var("NEPHERAL_SYSTEM_PROMPT")
            .ok()
            .filter(|p| !p.trim().is_empty())
    }

    pub async fn update_ai_config(
        &self,
        config: ai_engine::ModelConfig,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        *self.ai_model_config.write().await = config;
        if self.ai_engine.read().await.is_some() {
            self.initialize_ai_engine().await?;
        }
        Ok(())
    }

    async fn tuning_for_current_model(
        &self,
    ) -> Option<crate::nepheral_config::InferenceTuningConfig> {
        let path = self.ai_model_config.read().await.model_path.clone()?;
        let path = PathBuf::from(path);
        let bytes = std::fs::metadata(&path).ok()?.len();
        let kv_per_token = ai_engine::gguf_probe::probe(&path).and_then(|i| i.kv_bytes_per_token);
        let ram_mb = ai_engine::hw_detect::detect().ram_mb;
        let t = ai_engine::hw_detect::tuning_for_model(bytes, kv_per_token, ram_mb);
        Some(crate::nepheral_config::InferenceTuningConfig {
            n_ctx: t.n_ctx,
            n_ubatch: t.n_ubatch,
            max_new_tokens: t.max_new_tokens,
            estimated_ram_mb: t.estimated_ram_mb,
        })
    }

    async fn teardown_ai_engine(&self) {
        let extras = std::mem::take(&mut *self.extra_engines.write().await);
        for e in &extras {
            e.shutdown().await;
        }
        let Some(previous) = self.ai_engine.write().await.take() else {
            return;
        };
        previous.shutdown().await;
        const STOP_TIMEOUT: Duration = Duration::from_secs(90);
        let deadline = std::time::Instant::now() + STOP_TIMEOUT;
        while *self.inference_busy.lock().await {
            if std::time::Instant::now() >= deadline {
                tracing::warn!(
                    "The previous reply did not stop in time; loading the next model anyway"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(previous);
        if let Ok(mut cache) = self.ai_engine_status_cache.lock() {
            *cache = "switching".to_string();
        }
    }

    async fn record_ai_engine_failure(&self, reason: &str) {
        tracing::error!(error = %reason, "AI engine did not start");
        if let Ok(mut cache) = self.ai_engine_status_cache.lock() {
            *cache = format!("error: {reason}");
        }
        self.log(format!("AI engine failed to start: {reason}"))
            .await;
    }

    #[cfg(feature = "client")]
    pub fn ai_engine_status(&self) -> String {
        self.ai_engine_status_cache
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "not_initialized".to_string())
    }

    pub async fn decay_ai_sessions(&self) {
        if let Some(engine) = self.ai_engine.read().await.as_ref() {
            engine.decay_sessions().await;
        }
    }

    pub async fn run_ai_startup(self) {
        let mut attempts = 0u32;
        loop {
            if self.ai_engine.read().await.is_some() {
                self.announce_capabilities().await;
                return;
            }
            let has_source = Self::local_model_path().is_some()
                || !self
                    .ai_model_config
                    .read()
                    .await
                    .backend_url
                    .trim()
                    .is_empty();
            if has_source {
                match self.start_ai_engine().await {
                    Ok(()) => {
                        self.announce_capabilities().await;
                        return;
                    }
                    Err(e) => {
                        attempts += 1;
                        if attempts >= 10 {
                            tracing::warn!(error = %e, "AI engine failed to initialize after 10 attempts");
                            return;
                        }
                        tracing::warn!(error = %e, attempt = attempts, "AI engine not ready, retrying");
                    }
                }
            }
            if self.sleep_or_shutdown(Duration::from_secs(15)).await {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_counts_are_read_from_names() {
        assert_eq!(params_b_from_name("Qwen3-4B-Q4_K_M"), 4);
        assert_eq!(params_b_from_name("qwen2.5-0.5b-instruct"), 1);
        assert_eq!(params_b_from_name("Llama-3.1-70B-Instruct"), 70);
        assert_eq!(params_b_from_name("mystery-model"), 1);
        assert_eq!(params_b_from_name("phi-3-mini-128k"), 1);
    }

    #[test]
    fn a_model_is_named_after_its_file_unless_named_explicitly() {
        let path = "/var/lib/nepheral/.nepheral/models/qwen2.5-3b-instruct-q4_k_m.gguf";
        let name = model_name_for(path, None);
        assert_eq!(name, "qwen2.5-3b-instruct-q4_k_m");
        assert_eq!(params_b_from_name(&name), 3, "the size it earns as");
        assert_eq!(model_name_for(path, Some("  ".into())), name);
        assert_eq!(model_name_for(path, Some("my-model".into())), "my-model");
        assert_eq!(model_name_for("", None), "local");
    }

    #[test]
    fn tiers_follow_parameter_counts() {
        assert_eq!(tier_for_params(1), 0);
        assert_eq!(tier_for_params(8), 2);
        assert_eq!(tier_for_params(14), 3);
        assert_eq!(tier_for_params(72), 5);
    }
}
