// SPDX-License-Identifier: BUSL-1.1
pub use inner::*;

mod inner {
    use std::sync::OnceLock;

    use sysinfo::System;

    #[derive(Debug, Clone, PartialEq)]
    pub enum GpuKind {
        None,
        Vulkan,
        Cuda,
        Metal,
    }

    /// What this machine can run locally, and therefore which model it
    /// installs — see [`ModelTier::model_repo_file`].
    #[derive(Debug, Clone, PartialEq)]
    pub enum ModelTier {
        Minimum,     // Phi-3-mini-4k Q4     — 5 GB RAM
        Standard,    // Qwen3-4B Q4_K_M      — 6 GB RAM
        Performance, // Qwen3-8B Q4_K_M      — 10 GB RAM
        High,        // Qwen3-14B Q4_K_M     — 10 GB VRAM
        Maximum,     // Qwen3-30B-A3B Q4_K_M — 20 GB VRAM
        /// Too little memory for any local model — the node routes to an
        /// external backend instead of installing something that cannot run.
        External,
    }

    #[derive(Debug, Clone)]
    pub struct HardwareProfile {
        pub cpu_cores: usize,
        pub ram_mb: u64,
        pub gpu_kind: GpuKind,
        pub vram_mb: Option<u64>,
        /// True when `vram_mb` is carved out of system RAM (integrated
        /// graphics, Apple unified memory) rather than dedicated.
        pub gpu_shared_memory: bool,
        pub recommended_tier: ModelTier,
    }

    pub fn detect() -> HardwareProfile {
        let mut sys = System::new_all();
        sys.refresh_all();

        let ram_mb = sys.total_memory() / (1024 * 1024);
        let cpu_cores = sys.cpus().len();

        let (gpu_kind, vram_mb, gpu_shared_memory) = probe_gpu();

        let recommended_tier = recommend_tier(ram_mb, &gpu_kind, vram_mb, gpu_shared_memory);

        HardwareProfile {
            cpu_cores,
            ram_mb,
            gpu_kind,
            vram_mb,
            gpu_shared_memory,
            recommended_tier,
        }
    }

    // Cached: the first probe initializes the llama.cpp backend, which must
    // not repeat on every detect() call.
    static GPU_PROBE: OnceLock<(GpuKind, Option<u64>, bool)> = OnceLock::new();

    fn probe_gpu() -> (GpuKind, Option<u64>, bool) {
        GPU_PROBE.get_or_init(probe_gpu_uncached).clone()
    }

    fn probe_gpu_uncached() -> (GpuKind, Option<u64>, bool) {
        #[cfg(feature = "local-inference")]
        {
            if let Some((kind, vram_mb, shared)) = enumerate_gpu() {
                return (kind, Some(vram_mb), shared);
            }
        }

        #[cfg(target_os = "macos")]
        {
            // Apple Silicon is unified memory: the GPU's pool is system RAM.
            return match vram_override() {
                Some(mb) => (GpuKind::Metal, Some(mb), false),
                None => (GpuKind::Metal, Some(4096), true),
            };
        }

        #[allow(unreachable_code)]
        (GpuKind::None, None, false)
    }

    /// Reads `NEPHERAL_VRAM_MB`, which overrides whatever a probe below
    /// would otherwise report -- useful for testing tier selection without
    /// the hardware in hand, or correcting a driver that misreports VRAM.
    /// Only called from enumerate_gpu (local-inference builds) and from the
    /// macOS fallback below; a plain check build with neither has no caller.
    #[cfg(any(feature = "local-inference", target_os = "macos"))]
    fn vram_override() -> Option<u64> {
        std::env::var("NEPHERAL_VRAM_MB")
            .ok()
            .and_then(|v| v.parse().ok())
    }

    /// Enumerates GGML backend devices to find a real GPU, rather than
    /// guessing from SDK environment variables or DLL presence on disk --
    /// both of which report an SDK or driver being installed, not a GPU
    /// backend actually being linked into this binary, and neither of
    /// which can report actual VRAM.
    #[cfg(feature = "local-inference")]
    fn enumerate_gpu() -> Option<(GpuKind, u64, bool)> {
        use llama_cpp_2::{list_llama_ggml_backend_devices, LlamaBackendDeviceType};

        // Device enumeration requires the backend (and, on a dynamic-backends
        // build, the loadable CUDA/Vulkan modules) to already be initialized.
        crate::local_backend::ensure_backend().ok()?;

        let devices = list_llama_ggml_backend_devices();
        let gpu = devices
            .iter()
            .filter(|d| {
                matches!(
                    d.device_type,
                    LlamaBackendDeviceType::Gpu | LlamaBackendDeviceType::IntegratedGpu
                )
            })
            .max_by_key(|d| d.memory_total)?;

        let backend = gpu.backend.to_ascii_lowercase();
        let kind = if backend.starts_with("cuda") {
            GpuKind::Cuda
        } else if backend.starts_with("vulkan") {
            GpuKind::Vulkan
        } else if backend.starts_with("metal") {
            GpuKind::Metal
        } else {
            return None;
        };

        // An integrated GPU reports a slice of system RAM as its memory.
        // An explicit NEPHERAL_VRAM_MB is the operator asserting a real
        // figure, so it is taken as dedicated — that is what makes the
        // override usable for exercising the higher tiers on a laptop.
        let override_mb = vram_override();
        let shared = override_mb.is_none()
            && matches!(gpu.device_type, LlamaBackendDeviceType::IntegratedGpu);
        let vram_mb = override_mb.unwrap_or(gpu.memory_total as u64 / (1024 * 1024));
        tracing::info!(
            device = %gpu.description,
            backend = %gpu.backend,
            vram_mb,
            shared,
            "GPU detected"
        );
        Some((kind, vram_mb, shared))
    }

    fn recommend_tier(
        ram_mb: u64,
        gpu_kind: &GpuKind,
        vram_mb: Option<u64>,
        gpu_shared_memory: bool,
    ) -> ModelTier {
        let tier = tier_from(ram_mb, gpu_kind, vram_mb);
        if !gpu_shared_memory {
            return tier;
        }
        // Shared memory: the weights still occupy system RAM, so the VRAM
        // figure cannot justify a bigger model than RAM alone would.
        let ram_tier = tier_from(ram_mb, &GpuKind::None, None);
        if ram_tier.capacity_rank() < tier.capacity_rank() {
            ram_tier
        } else {
            tier
        }
    }

    fn tier_from(ram_mb: u64, gpu_kind: &GpuKind, vram_mb: Option<u64>) -> ModelTier {
        match (gpu_kind, vram_mb) {
            (GpuKind::None, _) | (_, None) => {
                if ram_mb >= 10 * 1024 {
                    ModelTier::Performance
                } else if ram_mb >= 6 * 1024 {
                    ModelTier::Standard
                } else if ram_mb >= 5 * 1024 {
                    ModelTier::Minimum
                } else {
                    ModelTier::External
                }
            }
            (_, Some(vram)) => {
                if vram >= 20 * 1024 {
                    ModelTier::Maximum
                } else if vram >= 10 * 1024 {
                    ModelTier::High
                } else if vram >= 6 * 1024 {
                    ModelTier::Performance
                } else if vram >= 4 * 1024 {
                    ModelTier::Standard
                } else {
                    ModelTier::Minimum
                }
            }
        }
    }

    impl ModelTier {
        /// Ordering by how much machine a tier demands, so two independently
        /// derived tiers can be compared and the smaller one taken.
        fn capacity_rank(&self) -> u8 {
            match self {
                ModelTier::External => 0,
                ModelTier::Minimum => 1,
                ModelTier::Standard => 2,
                ModelTier::Performance => 3,
                ModelTier::High => 4,
                ModelTier::Maximum => 5,
            }
        }

        pub fn model_name(&self) -> &'static str {
            match self {
                ModelTier::Minimum => "Phi-3-mini-4k Q4",
                ModelTier::Standard => "Qwen3-4B Q4_K_M",
                ModelTier::Performance => "Qwen3-8B Q4_K_M",
                ModelTier::High => "Qwen3-14B Q4_K_M",
                ModelTier::Maximum => "Qwen3-30B-A3B Q4_K_M",
                ModelTier::External => "External",
            }
        }

        /// The GGUF this tier installs, as `(repo, file)` on HuggingFace.
        ///
        /// The tier used to be advisory only — it was shown to the user while
        /// the installer downloaded one hardcoded model regardless, so what a
        /// machine was told it would get was not what it got.
        pub fn model_repo_file(&self) -> Option<(&'static str, &'static str)> {
            match self {
                ModelTier::Minimum => Some((
                    "microsoft/Phi-3-mini-4k-instruct-gguf",
                    "Phi-3-mini-4k-instruct-q4.gguf",
                )),
                ModelTier::Standard => Some(("Qwen/Qwen3-4B-GGUF", "Qwen3-4B-Q4_K_M.gguf")),
                ModelTier::Performance => Some(("Qwen/Qwen3-8B-GGUF", "Qwen3-8B-Q4_K_M.gguf")),
                ModelTier::High => Some(("Qwen/Qwen3-14B-GGUF", "Qwen3-14B-Q4_K_M.gguf")),
                ModelTier::Maximum => {
                    Some(("Qwen/Qwen3-30B-A3B-GGUF", "Qwen3-30B-A3B-Q4_K_M.gguf"))
                }
                ModelTier::External => None,
            }
        }

        pub fn model_url(&self) -> Option<String> {
            self.model_repo_file()
                .map(|(repo, file)| format!("https://huggingface.co/{repo}/resolve/main/{file}"))
        }

        pub fn min_ram_mb(&self) -> Option<u64> {
            match self {
                // Phi-3-mini is a ~2.4 GB file; 3 GB of total RAM cannot hold
                // it plus a context and an OS. The floor is a model that
                // actually answers rather than a tiny one that produces
                // nonsense — below this a machine uses an external backend.
                ModelTier::Minimum => Some(5 * 1024),
                ModelTier::Standard => Some(6 * 1024),
                ModelTier::Performance => Some(10 * 1024),
                ModelTier::High | ModelTier::Maximum => None,
                ModelTier::External => None,
            }
        }

        pub fn min_vram_mb(&self) -> Option<u64> {
            match self {
                ModelTier::High => Some(10 * 1024),
                ModelTier::Maximum => Some(20 * 1024),
                ModelTier::Performance => Some(6 * 1024),
                ModelTier::Standard => Some(4 * 1024),
                _ => None,
            }
        }

        /// Download size of this tier's GGUF, rounded from the published
        /// file sizes.
        pub fn approx_size_mb(&self) -> u64 {
            match self {
                ModelTier::Minimum => 2283,
                ModelTier::Standard => 2382,
                ModelTier::Performance => 4795,
                ModelTier::High => 8585,
                ModelTier::Maximum => 17697,
                ModelTier::External => 0,
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct InferenceTuning {
        pub n_ctx: u32,
        pub n_ubatch: u32,
        pub max_new_tokens: u32,
        pub estimated_ram_mb: u64,
    }

    /// Memory a machine can give inference, after the operating system and a
    /// margin for the application itself.
    pub fn inference_budget_mb(ram_mb: u64) -> u64 {
        inference_budget_mb_with(ram_mb, os_overhead_mib())
    }

    fn inference_budget_mb_with(ram_mb: u64, os_overhead: u64) -> u64 {
        const APP_AND_SAFETY_MIB: u64 = 1536;
        ram_mb
            .saturating_sub(os_overhead)
            .saturating_sub(APP_AND_SAFETY_MIB)
    }

    fn os_overhead_mib() -> u64 {
        match std::env::consts::OS {
            "windows" => 4096,
            "macos" => 3072,
            "linux" => 2048,
            _ => 3072,
        }
    }

    /// Context and batch sizes for one specific model on this machine.
    ///
    /// The cost of a token of context is a property of the model, not of the
    /// machine: it is layers x KV heads x (key + value) x 2 bytes, which
    /// ranges from a few kilobytes on a flash-attention model to well over a
    /// hundred on a conventional one — a factor of forty between two files a
    /// user might switch between. A figure derived once from the hardware and
    /// stored was therefore wrong for every model but the one it was computed
    /// for, and it was applied to all of them.
    ///
    /// `kv_bytes_per_token` comes from the model's own header. Without it,
    /// the conservative end of the observed range is assumed rather than the
    /// convenient one.
    pub fn tuning_for_model(
        model_bytes: u64,
        kv_bytes_per_token: Option<u64>,
        ram_mb: u64,
    ) -> InferenceTuning {
        tuning_for_model_with(model_bytes, kv_bytes_per_token, ram_mb, os_overhead_mib())
    }

    fn tuning_for_model_with(
        model_bytes: u64,
        kv_bytes_per_token: Option<u64>,
        ram_mb: u64,
        os_overhead: u64,
    ) -> InferenceTuning {
        const KV_UNKNOWN_BYTES_PER_TOKEN: u64 = 160 * 1024;
        const COMPUTE_PER_UBATCH_KIB: u64 = 512;
        const N_CTX_MIN: u32 = 1024;
        const N_CTX_MAX: u32 = 16384;
        const N_UBATCH_MIN: u32 = 128;
        const N_UBATCH_MAX: u32 = 1024;

        let model_mib = model_bytes / (1024 * 1024);
        let kv_per_token = kv_bytes_per_token
            .unwrap_or(KV_UNKNOWN_BYTES_PER_TOKEN)
            .max(1);
        let budget_mib = inference_budget_mb_with(ram_mb, os_overhead).saturating_sub(model_mib);

        if budget_mib < 256 {
            tracing::warn!(
                ram_mb,
                model_mib,
                "The model leaves too little memory for a tuned context; using the smallest one"
            );
            return InferenceTuning {
                n_ctx: N_CTX_MIN,
                n_ubatch: N_UBATCH_MIN,
                max_new_tokens: 512,
                estimated_ram_mb: model_mib + 128,
            };
        }

        let kv_budget_bytes = budget_mib * 1024 * 1024 * 7 / 10;
        let compute_budget_kib = budget_mib * 1024 * 3 / 10;

        let n_ctx =
            round_down_pow2((kv_budget_bytes / kv_per_token) as u32).clamp(N_CTX_MIN, N_CTX_MAX);
        let n_ubatch = round_down_pow2((compute_budget_kib / COMPUTE_PER_UBATCH_KIB) as u32)
            .clamp(N_UBATCH_MIN, N_UBATCH_MAX);

        // A reply may not claim more than a quarter of the window: the prompt
        // has to fit in the rest, and reserving more than the context can
        // hold made the reservation outgrow its own window.
        let max_new_tokens = (n_ctx / 4).clamp(256, 4096);
        let kv_mib = (n_ctx as u64).saturating_mul(kv_per_token) / (1024 * 1024);

        tracing::info!(
            model_mib,
            kv_bytes_per_token = kv_per_token,
            derived_from_header = kv_bytes_per_token.is_some(),
            n_ctx,
            n_ubatch,
            kv_mib,
            "Inference sized for this model"
        );

        InferenceTuning {
            n_ctx,
            n_ubatch,
            max_new_tokens,
            estimated_ram_mb: model_mib + kv_mib,
        }
    }

    pub fn recommend_tuning(profile: &HardwareProfile, tier: &ModelTier) -> InferenceTuning {
        const KV_PER_TOKEN_KIB: u64 = 100; // Qwen3-4B fp16 KV: ~0.1 MiB/token
        const COMPUTE_PER_UBATCH_KIB: u64 = 512; // Empirical: ~0.5 MiB per ubatch unit
        const N_CTX_MIN: u32 = 1024;
        const N_CTX_MAX: u32 = 16384;
        const N_UBATCH_MIN: u32 = 128;
        const N_UBATCH_MAX: u32 = 1024;
        const SAFETY_MARGIN_MIB: u64 = 1536;

        let os_overhead_mib: u64 = match std::env::consts::OS {
            "windows" => 4096,
            "macos" => 3072,
            "linux" => 2048,
            _ => 3072,
        };

        let model_mib = tier.approx_size_mb();
        let budget_mib = profile
            .ram_mb
            .saturating_sub(os_overhead_mib)
            .saturating_sub(model_mib)
            .saturating_sub(SAFETY_MARGIN_MIB);

        if budget_mib < 256 {
            tracing::warn!(
                ram_mb = profile.ram_mb,
                os_overhead_mib,
                model_mib,
                "Insufficient RAM for tuned inference — using conservative defaults"
            );
            return InferenceTuning {
                n_ctx: N_CTX_MIN,
                n_ubatch: N_UBATCH_MIN,
                max_new_tokens: 512,
                estimated_ram_mb: 128,
            };
        }

        let kv_budget_kib = budget_mib * 1024 * 7 / 10;
        let compute_budget_kib = budget_mib * 1024 * 3 / 10;

        let n_ctx_raw = (kv_budget_kib / KV_PER_TOKEN_KIB) as u32;
        let n_ctx = round_down_pow2(n_ctx_raw).clamp(N_CTX_MIN, N_CTX_MAX);

        let n_ubatch_raw = (compute_budget_kib / COMPUTE_PER_UBATCH_KIB) as u32;
        let n_ubatch = round_down_pow2(n_ubatch_raw).clamp(N_UBATCH_MIN, N_UBATCH_MAX);

        let max_new_tokens = ((n_ctx * 3) / 4).min(4096);

        let estimated_ram_mb = (n_ctx as u64 * KV_PER_TOKEN_KIB / 1024)
            + (n_ubatch as u64 * COMPUTE_PER_UBATCH_KIB / 1024);

        tracing::info!(
            ram_mb = profile.ram_mb,
            os_overhead_mib,
            model_mib,
            budget_mib,
            n_ctx,
            n_ubatch,
            max_new_tokens,
            estimated_ram_mb,
            "Inference tuning computed"
        );

        InferenceTuning {
            n_ctx,
            n_ubatch,
            max_new_tokens,
            estimated_ram_mb,
        }
    }

    fn round_down_pow2(mut v: u32) -> u32 {
        if v == 0 {
            return 0;
        }
        let mut p = 1u32;
        v >>= 1;
        while v > 0 {
            p <<= 1;
            v >>= 1;
        }
        p
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn integrated_gpu_does_not_inflate_tier() {
            let ram_mb = 32 * 1024;
            let shared = recommend_tier(ram_mb, &GpuKind::Vulkan, Some(16 * 1024), true);
            let dedicated = recommend_tier(ram_mb, &GpuKind::Vulkan, Some(16 * 1024), false);
            assert_eq!(shared, ModelTier::Performance);
            assert_eq!(dedicated, ModelTier::High);
        }

        #[test]
        fn every_runnable_tier_installs_a_real_model() {
            for tier in [
                ModelTier::Minimum,
                ModelTier::Standard,
                ModelTier::Performance,
                ModelTier::High,
                ModelTier::Maximum,
            ] {
                let (repo, file) = tier
                    .model_repo_file()
                    .unwrap_or_else(|| panic!("{tier:?} has no model"));
                assert!(repo.contains('/'), "{tier:?}: repo must be org/name");
                assert!(file.ends_with(".gguf"), "{tier:?}: file must be a GGUF");
                assert_eq!(
                    tier.model_url().unwrap(),
                    format!("https://huggingface.co/{repo}/resolve/main/{file}")
                );
            }
            assert!(ModelTier::External.model_repo_file().is_none());
        }

        /// The floor is a model that answers coherently, not the smallest one
        /// that loads.
        #[test]
        fn minimum_tier_is_phi() {
            let (repo, _) = ModelTier::Minimum.model_repo_file().unwrap();
            assert!(repo.contains("Phi-3"), "got {repo}");
        }

        #[test]
        fn discrete_gpu_keeps_its_tier() {
            assert_eq!(
                recommend_tier(16 * 1024, &GpuKind::Cuda, Some(24 * 1024), false),
                ModelTier::Maximum
            );
        }

        #[test]
        fn shared_memory_clamps_down_to_ram() {
            // 16 GB of "VRAM" carved out of 6 GB of RAM is still 6 GB.
            assert_eq!(
                recommend_tier(6 * 1024, &GpuKind::Vulkan, Some(16 * 1024), true),
                ModelTier::Standard
            );
        }

        #[test]
        fn no_gpu_falls_back_to_ram() {
            assert_eq!(
                recommend_tier(10 * 1024, &GpuKind::None, None, false),
                ModelTier::Performance
            );
            assert_eq!(
                recommend_tier(2 * 1024, &GpuKind::None, None, false),
                ModelTier::External
            );
        }
    }

    #[test]
    fn a_cheap_cache_buys_a_bigger_context_than_an_expensive_one() {
        // The whole point: two models of the same size can differ by forty
        // times in what a token of context costs, so one figure cannot serve
        // both. These are the real numbers from a flash-attention model and a
        // conventional one.
        const RAM: u64 = 16064;
        const SIZE: u64 = 7 * 1024 * 1024 * 1024;
        let cheap = tuning_for_model_with(SIZE, Some(3_840), RAM, 4096);
        let dear = tuning_for_model_with(SIZE, Some(147_456), RAM, 4096);
        assert!(
            cheap.n_ctx > dear.n_ctx,
            "cheap {} should exceed dear {}",
            cheap.n_ctx,
            dear.n_ctx
        );
    }

    #[test]
    fn a_model_that_fills_the_machine_gets_the_smallest_context() {
        // A 10 GB model on a 16 GB machine leaves under a gigabyte once the
        // operating system is counted, which is what made loading fail with
        // sizes computed for a 2 GB one.
        let t = tuning_for_model_with(10 * 1024 * 1024 * 1024, Some(147_456), 16064, 4096);
        assert_eq!(t.n_ctx, 1024);
        assert_eq!(t.max_new_tokens, 512);
    }

    #[test]
    fn a_reply_never_claims_more_than_its_window() {
        for kv in [3_840u64, 147_456] {
            for size_gb in [1u64, 4, 7, 10] {
                let t = tuning_for_model(size_gb * 1024 * 1024 * 1024, Some(kv), 16064);
                assert!(
                    t.max_new_tokens < t.n_ctx,
                    "{} new vs {} ctx at {} GB",
                    t.max_new_tokens,
                    t.n_ctx,
                    size_gb
                );
            }
        }
    }

    #[test]
    fn an_unreadable_header_is_sized_conservatively() {
        // Without the model's geometry the expensive end of the range is
        // assumed: guessing cheap would hand out a context that does not fit.
        let known_cheap = tuning_for_model(4 * 1024 * 1024 * 1024, Some(3_840), 16064);
        let unknown = tuning_for_model(4 * 1024 * 1024 * 1024, None, 16064);
        assert!(unknown.n_ctx <= known_cheap.n_ctx);
    }

    #[test]
    fn more_memory_never_shrinks_the_context() {
        let mut previous = 0;
        for ram in [8192u64, 16064, 32768, 65536] {
            let t = tuning_for_model(4 * 1024 * 1024 * 1024, Some(147_456), ram);
            assert!(t.n_ctx >= previous, "shrank at {ram} MB");
            previous = t.n_ctx;
        }
    }
}
