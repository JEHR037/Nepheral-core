// SPDX-License-Identifier: BUSL-1.1
//! Live model catalog backed by the public HuggingFace API (unauthenticated
//! repo search and file tree, using `lfs.oid` as the published SHA-256).

use serde::{Deserialize, Serialize};

const HF_API: &str = "https://huggingface.co/api";
const USER_AGENT: &str = concat!("nepheral/", env!("CARGO_PKG_VERSION"), " (model catalog)");

/// Publishers surfaced by default. Anything outside this list is reachable
/// through explicit search, flagged as untrusted in the UI: a GGUF is parsed
/// by llama.cpp in-process, so the publisher is part of the trust boundary.
pub const TRUSTED_ORGS: &[&str] = &[
    "Qwen",
    "google",
    "meta-llama",
    "mistralai",
    "microsoft",
    "ggml-org",
    "unsloth",
    "bartowski",
    "lmstudio-community",
    "NousResearch",
    "HuggingFaceTB",
    "TheBloke",
];

#[derive(Debug)]
pub enum CatalogError {
    Http(reqwest::Error),
    Status(u16),
    Empty,
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Http(e) => write!(f, "network error talking to HuggingFace: {e}"),
            CatalogError::Status(s) => write!(f, "HuggingFace returned HTTP {s}"),
            CatalogError::Empty => write!(f, "no GGUF files in that repository"),
        }
    }
}
impl std::error::Error for CatalogError {}
impl From<reqwest::Error> for CatalogError {
    fn from(e: reqwest::Error) -> Self {
        CatalogError::Http(e)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoSummary {
    pub id: String,
    pub downloads: u64,
    pub likes: u64,
    /// Parameter count in billions, parsed from the repo name when present.
    pub params_b: Option<f32>,
    pub trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFile {
    pub repo: String,
    pub file: String,
    pub url: String,
    pub size_bytes: u64,
    /// SHA-256 as published by HuggingFace (LFS oid). `None` for files not
    /// stored in LFS — those are never offered for download.
    pub sha256_hex: Option<String>,
    /// Quantisation label parsed from the filename, e.g. `Q4_K_M`.
    pub quant: Option<String>,
    pub params_b: Option<f32>,
}

// ---------------------------------------------------------------- HTTP

fn client() -> Result<reqwest::Client, reqwest::Error> {
    crate::http_client()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(20))
        .build()
}

#[derive(Deserialize)]
struct HfRepo {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    likes: u64,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    gated: serde_json::Value,
}

/// Search GGUF text-generation repositories. `query` empty = most downloaded.
pub async fn search_repos(query: &str, limit: usize) -> Result<Vec<RepoSummary>, CatalogError> {
    let mut url = format!(
        "{HF_API}/models?filter=gguf&pipeline_tag=text-generation&sort=downloads&direction=-1&limit={limit}"
    );
    let q = query.trim();
    if !q.is_empty() {
        url.push_str(&format!("&search={}", urlencode(q)));
    }

    let resp = client()?.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(CatalogError::Status(resp.status().as_u16()));
    }
    let repos: Vec<HfRepo> = resp.json().await?;

    Ok(repos
        .into_iter()
        .filter(|r| !r.private && !is_gated(&r.gated))
        .map(|r| RepoSummary {
            params_b: parse_params_b(&r.id),
            trusted: is_trusted(&r.id),
            id: r.id,
            downloads: r.downloads,
            likes: r.likes,
        })
        .collect())
}

fn is_gated(v: &serde_json::Value) -> bool {
    // HF returns `false` or a string like "auto"/"manual" for gated repos.
    match v {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(_) => true,
        _ => false,
    }
}

#[derive(Deserialize)]
struct HfTreeEntry {
    path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    lfs: Option<HfLfs>,
}

#[derive(Deserialize)]
struct HfLfs {
    oid: String,
    #[serde(default)]
    size: u64,
}

/// Extract the `rel="next"` target from an RFC 8288 `Link` header value.
fn parse_next_link(header: &str) -> Option<String> {
    header
        .split(',')
        .find(|part| part.contains("rel=\"next\""))
        .and_then(|part| {
            let start = part.find('<')? + 1;
            let end = part[start..].find('>')? + start;
            Some(part[start..end].to_string())
        })
}

/// Follow HuggingFace's `Link: <...>; rel="next"` cursor to the next page
/// of a tree listing, if there is one.
fn next_page_url(resp: &reqwest::Response) -> Option<String> {
    let link = resp.headers().get(reqwest::header::LINK)?.to_str().ok()?;
    parse_next_link(link)
}

/// List the downloadable GGUF files of a repository (`main`), each with its
/// published SHA-256. Recurses and pages through the whole tree.
pub async fn list_files(repo: &str) -> Result<Vec<ModelFile>, CatalogError> {
    // Bounded so a malformed or cyclic Link header cannot spin forever;
    // 20 pages is far past any real GGUF repository.
    const MAX_PAGES: usize = 20;

    let client = client()?;
    let mut url = format!("{HF_API}/models/{repo}/tree/main?recursive=true");
    let mut entries: Vec<HfTreeEntry> = Vec::new();

    for _ in 0..MAX_PAGES {
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(CatalogError::Status(resp.status().as_u16()));
        }
        let next = next_page_url(&resp);
        entries.extend(resp.json::<Vec<HfTreeEntry>>().await?);
        match next {
            Some(n) => url = n,
            None => break,
        }
    }

    let repo_params = parse_params_b(repo);
    let mut files: Vec<ModelFile> = entries
        .into_iter()
        .filter(|e| e.path.to_ascii_lowercase().ends_with(".gguf"))
        // Multi-part shards ("-00001-of-00003.gguf") need assembly we don't do.
        .filter(|e| !e.path.contains("-of-"))
        // Vision projectors ship beside a model and are useless alone.
        // Listed as variants they looked like tiny quantisations of the
        // model itself — an 888 MB "F16" of a 27B — and offering one as
        // something to run was simply wrong.
        .filter(|e| !is_projector(&e.path))
        .map(|e| {
            let size = e
                .lfs
                .as_ref()
                .map(|l| l.size)
                .filter(|s| *s > 0)
                .unwrap_or(e.size);
            ModelFile {
                url: format!("https://huggingface.co/{repo}/resolve/main/{}", e.path),
                sha256_hex: e.lfs.as_ref().map(|l| l.oid.clone()),
                quant: parse_quant(&e.path),
                params_b: parse_params_b(&e.path).or(repo_params),
                repo: repo.to_string(),
                file: e.path,
                size_bytes: size,
            }
        })
        .filter(|f| f.sha256_hex.is_some())
        .collect();

    if files.is_empty() {
        return Err(CatalogError::Empty);
    }
    files.sort_by_key(|f| f.size_bytes);
    Ok(files)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------------------------------------------------------------- parsing

pub fn is_trusted(repo_id: &str) -> bool {
    let org = repo_id.split('/').next().unwrap_or("");
    TRUSTED_ORGS.iter().any(|t| t.eq_ignore_ascii_case(org))
}

/// Parameter count in billions from a repo or file name: `Qwen3-4B-GGUF` → 4.0,
/// `gemma-2-270m` → 0.27. Returns `None` when no size token is present.
pub fn parse_params_b(name: &str) -> Option<f32> {
    let bytes = name.as_bytes();
    let mut best: Option<f32> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // A size token must not be glued to a preceding letter/digit-word
        // ("Qwen3" must not read as 3B when followed by "-4B").
        let start = i;
        let mut j = i;
        let mut seen_dot = false;
        while j < bytes.len()
            && (bytes[j].is_ascii_digit()
                || (bytes[j] == b'.'
                    && !seen_dot
                    && j + 1 < bytes.len()
                    && bytes[j + 1].is_ascii_digit()))
        {
            if bytes[j] == b'.' {
                seen_dot = true;
            }
            j += 1;
        }
        if j < bytes.len() {
            let unit = bytes[j];
            let after_ok = j + 1 >= bytes.len() || !bytes[j + 1].is_ascii_alphanumeric();
            if after_ok {
                let num: f32 = name[start..j].parse().ok()?;
                let scaled = match unit {
                    b'B' | b'b' => Some(num),
                    b'M' | b'm' => Some(num / 1000.0),
                    _ => None,
                };
                if let Some(v) = scaled {
                    // Keep the largest plausible token: "Qwen3.6-27B" → 27.
                    if (0.01..=2000.0).contains(&v) && best.is_none_or(|b| v > b) {
                        best = Some(v);
                    }
                }
            }
        }
        i = j.max(start + 1);
    }
    best
}

/// Quantisation label from a GGUF filename: `Qwen3-4B-Q4_K_M.gguf` → `Q4_K_M`.
///
/// The whole token is returned, prefix included. Scanning for a bare `Q`
/// followed by a digit read `PQ2_0` as `Q2_0` and `PTQ1_0` as `Q1_0`, which
/// presented formats existing only in one publisher's llama.cpp fork as the
/// standard ones this build implements — a file that cannot run, offered
/// under the name of a file that can.
pub fn parse_quant(file: &str) -> Option<String> {
    let stem = file.strip_suffix(".gguf").unwrap_or(file);
    // Scan for a Q/IQ token and take it plus any _X suffixes.
    let upper = stem.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let is_q = bytes[i] == b'Q' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit();
        let is_iq = bytes[i] == b'I'
            && i + 2 < bytes.len()
            && bytes[i + 1] == b'Q'
            && bytes[i + 2].is_ascii_digit();
        if is_q || is_iq {
            // Letters immediately before the token belong to it. Digits
            // are not consumed, so "Llama3Q4_0" still reads as Q4_0.
            let mut start = i;
            while start > 0 && bytes[start - 1].is_ascii_alphabetic() {
                start -= 1;
            }
            let i = start;
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_')
                && !(j > i && bytes[j] == b'-')
            {
                j += 1;
            }
            return Some(upper[i..j].to_string());
        }
        i += 1;
    }
    if upper.contains("F16") {
        return Some("F16".to_string());
    }
    if upper.contains("BF16") {
        return Some("BF16".to_string());
    }
    None
}

/// True for companion files that are not models: multimodal projectors,
/// published as `mmproj-*.gguf` or `*-mmproj-*.gguf`.
fn is_projector(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.contains("mmproj")
}

/// How much of a published file to fetch before giving up on reading its
/// header. Tried in order: most vocabularies fit well inside the first, and
/// the larger read is only paid for by the few that do not.
const HEADER_FETCH_STEPS: [u64; 2] = [4 * 1024 * 1024, 20 * 1024 * 1024];

/// Reads a published file's header over HTTP, without downloading the model.
///
/// A GGUF states the storage format of every tensor in its first megabytes,
/// and HuggingFace serves byte ranges, so whether this build can load a file
/// is knowable before committing to gigabytes. Sizing a model against memory
/// says nothing about whether the engine implements its format, and offering
/// a file on that basis alone is how someone ends up downloading 7 GB of a
/// quantisation their llama.cpp has never heard of.
///
/// `None` when the file cannot be reached or is not a GGUF; the caller treats
/// that as "not known", never as "fine".
pub async fn probe_published_header(url: &str) -> Option<crate::gguf_probe::GgufInfo> {
    let client = client().ok()?;
    let mut last: Option<crate::gguf_probe::GgufInfo> = None;

    for bytes in HEADER_FETCH_STEPS {
        let resp = client
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes=0-{}", bytes - 1))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body = resp.bytes().await.ok()?;
        let info = crate::gguf_probe::parse(&body)?;
        // Enough was read once the tensor table has been reached; a longer
        // fetch would tell us nothing more.
        if info.tensor_types.is_some() {
            return Some(info);
        }
        last = Some(info);
        // A body shorter than asked for means the whole file is already here,
        // so reading further cannot help.
        if (body.len() as u64) < bytes {
            break;
        }
    }
    last
}

// ---------------------------------------------------------------- fit

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fit {
    /// Runs with room to spare.
    Comfortable,
    /// Fits, but with little headroom — expect swapping or a small context.
    Tight,
    /// Does not fit in this machine's memory.
    TooBig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitReport {
    pub fit: Fit,
    /// Memory the model needs: weights + KV cache + compute buffer + margin.
    pub needed_mb: u64,
    /// Memory we can actually give it (RAM minus OS overhead, or VRAM).
    pub available_mb: u64,
    /// Rough seconds for a medium reply. Order-of-magnitude, not a promise.
    pub est_secs_per_reply: u32,
    pub uses_gpu: bool,
}

/// Runtime overhead beyond the weights, at a usable 4k context.
const RUNTIME_OVERHEAD_MB: u64 = 660;
/// Minimum viable overhead (1k context, smallest micro-batch).
const RUNTIME_MIN_MB: u64 = 170;
const SAFETY_MARGIN_MB: u64 = 1024;

fn os_overhead_mb() -> u64 {
    match std::env::consts::OS {
        "windows" => 4096,
        "macos" => 3072,
        "linux" => 1536,
        _ => 3072,
    }
}

/// Decide whether a model of `size_bytes` can run on a machine with `ram_mb`
/// of system memory and optionally `vram_mb` of dedicated video memory.
pub fn assess(
    size_bytes: u64,
    params_b: Option<f32>,
    ram_mb: u64,
    vram_mb: Option<u64>,
) -> FitReport {
    let model_mb = size_bytes / (1024 * 1024);

    // GPU path: weights live in VRAM, so system RAM is no longer the limit.
    if let Some(vram) = vram_mb.filter(|v| *v > 0) {
        let needed = model_mb + RUNTIME_OVERHEAD_MB;
        if needed <= vram {
            return FitReport {
                fit: if needed + 512 <= vram {
                    Fit::Comfortable
                } else {
                    Fit::Tight
                },
                needed_mb: needed,
                available_mb: vram,
                est_secs_per_reply: est_secs(params_b, true),
                uses_gpu: true,
            };
        }
        // Too big for VRAM — fall through and judge it as a CPU/partial run.
    }

    let available = ram_mb.saturating_sub(os_overhead_mb());
    let comfortable_needed = model_mb + RUNTIME_OVERHEAD_MB + SAFETY_MARGIN_MB;
    let min_needed = model_mb + RUNTIME_MIN_MB + SAFETY_MARGIN_MB / 2;

    let fit = if comfortable_needed <= available {
        Fit::Comfortable
    } else if min_needed <= available {
        Fit::Tight
    } else {
        Fit::TooBig
    };

    FitReport {
        fit,
        needed_mb: comfortable_needed,
        available_mb: available,
        est_secs_per_reply: est_secs(params_b, false),
        uses_gpu: false,
    }
}

/// Rough wall-clock for a ~300-token reply. Anchored on measured CPU numbers
/// (1.7B ≈ 8 s, 4B ≈ 40 s, 8B ≈ 90 s); GPUs land roughly 8× faster.
fn est_secs(params_b: Option<f32>, gpu: bool) -> u32 {
    let p = params_b.unwrap_or(4.0).max(0.1);
    let cpu = (p * 11.0 - 10.0).max(3.0);
    let secs = if gpu { (cpu / 8.0).max(1.0) } else { cpu };
    secs.round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_link_extracted_from_link_header() {
        let h = "<https://huggingface.co/api/models/x/tree/main?cursor=ABC>; rel=\"next\"";
        assert_eq!(
            parse_next_link(h).as_deref(),
            Some("https://huggingface.co/api/models/x/tree/main?cursor=ABC")
        );
    }

    #[test]
    fn next_link_picked_out_of_several_rels() {
        let h = "<https://example.test/first>; rel=\"first\",                  <https://example.test/next>; rel=\"next\"";
        assert_eq!(
            parse_next_link(h).as_deref(),
            Some("https://example.test/next")
        );
    }

    #[test]
    fn last_page_has_no_next_link() {
        assert_eq!(
            parse_next_link("<https://example.test/prev>; rel=\"prev\""),
            None
        );
        assert_eq!(parse_next_link(""), None);
    }

    #[test]
    fn params_parsed_from_repo_and_file_names() {
        assert_eq!(parse_params_b("Qwen/Qwen3-4B-GGUF"), Some(4.0));
        assert_eq!(parse_params_b("Qwen3-4B-Q4_K_M.gguf"), Some(4.0));
        assert_eq!(parse_params_b("unsloth/Qwen3.6-27B-MTP-GGUF"), Some(27.0));
        assert_eq!(parse_params_b("Qwen3-1.7B-Q4_K_M.gguf"), Some(1.7));
        assert_eq!(parse_params_b("no-size-here"), None);
    }

    #[test]
    fn qwen_prefix_digit_is_not_a_size() {
        // "Qwen3" must not read as 3B — the 3 is glued to letters.
        assert_eq!(parse_params_b("Qwen3-1.7B"), Some(1.7));
    }

    #[test]
    fn million_scale_models_become_fractional_billions() {
        let v = parse_params_b("embeddinggemma-300M-GGUF").unwrap();
        assert!((v - 0.3).abs() < 1e-6, "got {v}");
    }

    #[test]
    fn quant_parsed_from_filename() {
        assert_eq!(
            parse_quant("Qwen3-4B-Q4_K_M.gguf").as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(parse_quant("Qwen3-4B-Q8_0.gguf").as_deref(), Some("Q8_0"));
        assert_eq!(parse_quant("model-IQ4_XS.gguf").as_deref(), Some("IQ4_XS"));
        assert_eq!(parse_quant("model-f16.gguf").as_deref(), Some("F16"));
    }

    #[test]
    fn trusted_orgs_recognised() {
        assert!(is_trusted("Qwen/Qwen3-4B-GGUF"));
        assert!(is_trusted("bartowski/whatever-GGUF"));
        assert!(!is_trusted("randomuser/sketchy-GGUF"));
    }

    #[test]
    fn small_model_fits_a_small_machine() {
        // 1.1 GB model on an 8 GB Linux box.
        let r = assess(1_100 * 1024 * 1024, Some(1.7), 8 * 1024, None);
        assert_eq!(r.fit, Fit::Comfortable);
        assert!(!r.uses_gpu);
    }

    #[test]
    fn huge_model_does_not_fit_a_small_machine() {
        // 18 GB model on an 8 GB box.
        let r = assess(18_000 * 1024 * 1024, Some(30.0), 8 * 1024, None);
        assert_eq!(r.fit, Fit::TooBig);
    }

    #[test]
    fn vram_lets_a_big_model_run() {
        // 8 GB model, 24 GB VRAM — comfortable and GPU-backed.
        let r = assess(8_000 * 1024 * 1024, Some(14.0), 16 * 1024, Some(24 * 1024));
        assert_eq!(r.fit, Fit::Comfortable);
        assert!(r.uses_gpu);
        assert!(r.est_secs_per_reply < 30);
    }

    #[test]
    fn model_too_big_for_vram_falls_back_to_ram_judgement() {
        // 18 GB model, 8 GB VRAM, 64 GB RAM: not GPU, but RAM can hold it.
        let r = assess(18_000 * 1024 * 1024, Some(30.0), 64 * 1024, Some(8 * 1024));
        assert!(!r.uses_gpu);
        assert_eq!(r.fit, Fit::Comfortable);
    }

    #[test]
    fn tight_sits_between_comfortable_and_too_big() {
        // Pick a size that clears the minimum but not the comfortable bar.
        let ram = 8 * 1024u64;
        let avail = ram - os_overhead_mb();
        let size_mb = avail - RUNTIME_MIN_MB - SAFETY_MARGIN_MB / 2;
        let r = assess(size_mb * 1024 * 1024, Some(7.0), ram, None);
        assert_eq!(r.fit, Fit::Tight);
    }

    #[test]
    fn a_fork_quantisation_keeps_its_prefix() {
        // PQ2_0 and PTQ1_0 exist only in one publisher's fork. Reported as
        // Q2_0 and Q1_0 they were indistinguishable from the formats this
        // build implements, so a file that cannot run was offered under the
        // name of one that can.
        assert_eq!(
            parse_quant("Ternary-Bonsai-2-27B-PQ2_0.gguf").as_deref(),
            Some("PQ2_0")
        );
        assert_eq!(
            parse_quant("Ternary-Bonsai-2-27B-PTQ1_0.gguf").as_deref(),
            Some("PTQ1_0")
        );
    }

    #[test]
    fn ordinary_quantisations_are_unchanged() {
        assert_eq!(
            parse_quant("Qwen3-4B-Q4_K_M.gguf").as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(parse_quant("model-IQ4_XS.gguf").as_deref(), Some("IQ4_XS"));
        assert_eq!(parse_quant("foo-Q8_0.gguf").as_deref(), Some("Q8_0"));
    }

    #[test]
    fn a_digit_before_the_token_is_not_swallowed() {
        assert_eq!(parse_quant("Llama3Q4_0.gguf").as_deref(), Some("Q4_0"));
    }

    #[test]
    fn projectors_are_not_models() {
        // An 888 MB "F16" of a 27B model was a vision projector listed as a
        // quantisation of the model beside it.
        assert!(is_projector("mmproj-BF16.gguf"));
        assert!(is_projector("mmproj-Q8_0.gguf"));
        assert!(is_projector("Ternary-Bonsai-2-27B-mmproj-BF16.gguf"));
        assert!(is_projector("some/dir/mmproj-F16.gguf"));
        assert!(!is_projector("Qwen3-4B-Q4_K_M.gguf"));
        assert!(!is_projector("Ternary-Bonsai-27B-Q2_g64.gguf"));
    }
}
