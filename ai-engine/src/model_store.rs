// SPDX-License-Identifier: BUSL-1.1
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

use reqwest::header::RANGE;
use sha2::{Digest, Sha256};

/// Below this a file cannot be a model at all: an HTML error page, a
/// truncated transfer, a stray fixture.
///
/// It is a floor on plausibility, not on model size. The previous 2 GB figure
/// was taken from the size of the default 4B model, which quietly made every
/// model below that class unusable over the mesh. What a file is gets decided
/// by reading its header ([`crate::gguf_probe`]), not by how big it is.
pub const MIN_PLAUSIBLE_MODEL_BYTES: u64 = 20 * 1024 * 1024;

/// The name a published file is stored under here.
///
/// A repository listing gives paths, not names: a file may sit in a
/// subdirectory ("dspark-abliterated/model.gguf"), and the path comes from a
/// repository anyone can publish, so it may also contain traversal. Joining
/// one straight onto the models directory wrote either into a directory that
/// does not exist — a download that failed after the whole transfer, leaving
/// nothing behind — or, with "..", outside the directory entirely.
///
/// Only the final component is kept, and models stay in one flat directory,
/// which is also the only place [`ModelStore::installed`] and the node's path
/// resolution look. `None` for anything that is not a plain file name.
pub fn local_file_name(published_path: &str) -> Option<String> {
    let name = published_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(published_path)
        .trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(':') {
        return None;
    }
    Some(name.to_string())
}

#[derive(Debug)]
pub enum ModelError {
    Io(std::io::Error),
    Http(reqwest::Error),
    HashMismatch { expected: [u8; 32], got: [u8; 32] },
    BadResponse { status: u16, content_type: String },
    TooSmall { bytes: u64 },
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Io(e) => write!(f, "io: {e}"),
            ModelError::Http(e) => write!(f, "http: {e}"),
            ModelError::HashMismatch { .. } => write!(f, "sha256 mismatch"),
            ModelError::BadResponse { status, content_type } =>
                write!(f, "server returned {status} ({content_type}) — wrong URL or rate-limited"),
            ModelError::TooSmall { bytes } =>
                write!(f, "downloaded file is {bytes} bytes, below the {} MB floor for anything that could be a model; most likely an error page", MIN_PLAUSIBLE_MODEL_BYTES / (1024 * 1024)),
        }
    }
}

impl std::error::Error for ModelError {}
impl From<std::io::Error> for ModelError {
    fn from(e: std::io::Error) -> Self {
        ModelError::Io(e)
    }
}
impl From<reqwest::Error> for ModelError {
    fn from(e: reqwest::Error) -> Self {
        ModelError::Http(e)
    }
}

#[derive(Debug, Clone)]
pub struct InstalledModel {
    pub file_name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

pub fn decode_sha256_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim().strip_prefix("sha256:").unwrap_or(s.trim());
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(s, &mut out).ok()?;
    Some(out)
}

/// Splits a Hugging Face "resolve" URL into `(repo, file)`, e.g.
/// `huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf` ->
/// `("Qwen/Qwen3-4B-GGUF", "Qwen3-4B-Q4_K_M.gguf")`.
pub fn parse_hf_url(url: &str) -> Option<(String, String)> {
    let rest = url.split_once("://")?.1;
    let rest = rest.strip_prefix("huggingface.co/")?;
    let (repo, after_resolve) = rest.split_once("/resolve/")?;
    let (_rev, file) = after_resolve.split_once('/')?;
    let file = file.split('?').next()?;
    if repo.is_empty() || file.is_empty() {
        return None;
    }
    Some((repo.to_string(), file.to_string()))
}

/// Stream a file through a hasher, returning the byte count.
fn hash_file_into(path: &Path, hasher: &mut Sha256) -> Result<u64, std::io::Error> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok(total)
}

pub struct ModelStore {
    base_dir: PathBuf,
}

impl ModelStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    pub fn default_dir() -> PathBuf {
        dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".nepheral")
    }

    /// The pre-multi-model location. Only `migrate_legacy_model` still
    /// looks here; downloads go to `models/<publisher file name>`.
    fn legacy_model_path(&self) -> PathBuf {
        self.base_dir.join("model.gguf")
    }

    /// Directory holding every downloaded model. Multiple GGUFs can live
    /// side by side; the active one is recorded in config.json.
    pub fn models_dir(&self) -> PathBuf {
        self.base_dir.join("models")
    }

    /// One-time move of the pre-multi-model `<base>/model.gguf` into
    /// `models/model.gguf`. Idempotent; a failed rename is logged, not fatal.
    pub fn migrate_legacy_model(&self) {
        let legacy = self.legacy_model_path();
        if !legacy.exists() {
            return;
        }
        let dir = self.models_dir();
        let target = dir.join("model.gguf");
        if target.exists() {
            // Something already occupies the destination name — don't clobber it.
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(error = %e, dir = %dir.display(), "legacy model migration: could not create models/ dir");
            return;
        }
        match std::fs::rename(&legacy, &target) {
            Ok(()) => tracing::info!(
                from = %legacy.display(),
                to = %target.display(),
                "Migrated legacy model.gguf into models/"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                from = %legacy.display(),
                "Legacy model migration failed — will retry next time"
            ),
        }
    }

    /// Every GGUF currently on disk. Migrates the legacy single `model.gguf`
    /// (from before multi-model support) into `models/` first, so there is
    /// exactly one on-disk representation to scan instead of two locations
    /// that need to be kept in sync forever.
    pub fn installed(&self) -> Vec<InstalledModel> {
        self.migrate_legacy_model();
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(self.models_dir()) {
            for e in entries.flatten() {
                let path = e.path();
                let is_gguf = path
                    .extension()
                    .map(|x| x.eq_ignore_ascii_case("gguf"))
                    .unwrap_or(false);
                if !is_gguf {
                    continue;
                }
                let size_bytes = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(InstalledModel {
                    file_name: path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    path,
                    size_bytes,
                });
            }
        }
        out.sort_by(|a, b| a.file_name.cmp(&b.file_name));
        out
    }

    /// Download `url` into `models/<file_name>`, verifying against the
    /// SHA-256 the publisher advertises (HuggingFace exposes it as the LFS
    /// oid). Resumes a partial file and hashes as it writes, so a 3 GB
    /// model is never loaded into memory just to be checked.
    pub async fn download_model(
        &self,
        url: &str,
        file_name: &str,
        expected_sha256_hex: Option<&str>,
        progress: impl Fn(u64, u64) + Send + 'static,
    ) -> Result<PathBuf, ModelError> {
        let name = local_file_name(file_name).ok_or_else(|| {
            ModelError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{file_name} is not a usable file name"),
            ))
        })?;
        let dir = self.models_dir();
        std::fs::create_dir_all(&dir)?;
        let final_path = dir.join(&name);
        let partial_path = dir.join(format!("{name}.part"));

        let client = crate::http_client()
            .user_agent(concat!(
                "nepheral/",
                env!("CARGO_PKG_VERSION"),
                " (GGUF downloader)"
            ))
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        // Resume: re-hash whatever is already on disk before appending.
        let mut hasher = Sha256::new();
        let existing_bytes = if partial_path.exists() {
            let n = hash_file_into(&partial_path, &mut hasher)?;
            tracing::info!(offset = n, file = file_name, "Resuming model download");
            n
        } else {
            0
        };

        let mut request = client.get(url);
        if existing_bytes > 0 {
            request = request.header(RANGE, format!("bytes={existing_bytes}-"));
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        if content_type.contains("text/html") || content_type.contains("application/json") {
            return Err(ModelError::BadResponse {
                status,
                content_type,
            });
        }
        if status == 200 && existing_bytes > 0 {
            // Server ignored the range request — start over so the hash matches.
            hasher = Sha256::new();
            let _ = std::fs::remove_file(&partial_path);
        } else if status != 200 && status != 206 {
            return Err(ModelError::BadResponse {
                status,
                content_type,
            });
        }
        let restarted = status == 200 && existing_bytes > 0;
        let base = if restarted { 0 } else { existing_bytes };

        let content_length = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let total = base + content_length;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&partial_path)?;

        let mut downloaded = base;
        let mut response = response;
        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk)?;
            hasher.update(&chunk);
            downloaded += chunk.len() as u64;
            progress(downloaded, total);
        }
        drop(file);

        let written = std::fs::metadata(&partial_path)?.len();
        if written < MIN_PLAUSIBLE_MODEL_BYTES {
            let _ = std::fs::remove_file(&partial_path);
            return Err(ModelError::TooSmall { bytes: written });
        }

        if let Some(hex_expected) = expected_sha256_hex {
            let got: [u8; 32] = hasher.finalize().into();
            let expected = decode_sha256_hex(hex_expected).ok_or(ModelError::BadResponse {
                status: 0,
                content_type: "malformed publisher checksum".to_string(),
            })?;
            if got != expected {
                let _ = std::fs::remove_file(&partial_path);
                return Err(ModelError::HashMismatch { expected, got });
            }
            tracing::info!(
                file = file_name,
                "Model SHA-256 verified against publisher checksum"
            );
        } else {
            tracing::warn!(
                file = file_name,
                "Publisher did not advertise a checksum — file integrity unverified"
            );
        }

        std::fs::rename(&partial_path, &final_path)?;
        Ok(final_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_is_kept() {
        assert_eq!(
            local_file_name("Qwen3-4B-Q4_K_M.gguf").as_deref(),
            Some("Qwen3-4B-Q4_K_M.gguf")
        );
    }

    #[test]
    fn a_published_subdirectory_is_dropped() {
        // Repositories publish nested paths. Joining one onto the models
        // directory wrote into a directory that does not exist, so the
        // download failed only after transferring the whole file.
        assert_eq!(
            local_file_name("dspark-abliterated/dspark-DeepSeek-V4-Flash-0731-Q8_0.gguf")
                .as_deref(),
            Some("dspark-DeepSeek-V4-Flash-0731-Q8_0.gguf")
        );
        assert_eq!(
            local_file_name("a/b/c/model.gguf").as_deref(),
            Some("model.gguf")
        );
    }

    #[test]
    fn traversal_cannot_escape_the_models_directory() {
        // The path comes from a repository listing, which anyone can publish.
        // Joined directly, "../.." would have written outside the directory
        // this store owns.
        assert_eq!(
            local_file_name("../../evil.gguf").as_deref(),
            Some("evil.gguf")
        );
        assert_eq!(local_file_name("..").as_deref(), None);
        assert_eq!(local_file_name("a/..").as_deref(), None);
        assert_eq!(local_file_name(".").as_deref(), None);
    }

    #[test]
    fn a_windows_separator_is_a_separator_too() {
        assert_eq!(
            local_file_name(r"dir\model.gguf").as_deref(),
            Some("model.gguf")
        );
        assert_eq!(
            local_file_name(r"..\..\evil.gguf").as_deref(),
            Some("evil.gguf")
        );
    }

    #[test]
    fn a_drive_letter_is_refused() {
        // "C:evil.gguf" is a drive-relative path on Windows, not a name.
        assert_eq!(local_file_name("C:evil.gguf"), None);
    }

    #[test]
    fn nothing_usable_yields_nothing() {
        assert_eq!(local_file_name(""), None);
        assert_eq!(local_file_name("   "), None);
        assert_eq!(local_file_name("dir/"), None);
    }
}
