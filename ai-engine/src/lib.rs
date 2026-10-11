// SPDX-License-Identifier: BUSL-1.1
pub mod backend;
pub mod device_choice;
pub mod external_backend;
pub mod gguf_probe;
pub mod hw_detect;
pub mod inference_engine;
pub mod local_backend;
pub mod model_catalog;
pub mod model_store;
pub mod redaction;
pub mod reply_budget;
pub mod types;

pub use backend::InferenceBackend;
pub use external_backend::ExternalHttpBackend;
pub use inference_engine::InferenceEngine;
pub use inference_engine::StatelessReply;
pub use model_catalog::{Fit, FitReport, ModelFile, RepoSummary};
pub use model_store::{ModelError, ModelStore, MIN_PLAUSIBLE_MODEL_BYTES};
pub use types::*;

// Hardware detection is plain sysinfo + probes: available in every build,
// so headless servers can size a model without the llama.cpp toolchain.
pub use hw_detect::{GpuKind, HardwareProfile, InferenceTuning, ModelTier};

#[cfg(feature = "local-inference")]
pub use local_backend::LlamaLocalBackend;

/// A builder for every HTTP client the app makes. reqwest is built without a
/// TLS crypto provider of its own and panics when none is set, so ring, the
/// provider the mesh transport already uses, is installed as the process
/// default first.
pub fn http_client() -> reqwest::ClientBuilder {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // Another thread may install one first; either way one is set.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    reqwest::Client::builder()
}

/// How many tensor storage formats the ggml compiled into this build knows,
/// or `None` when no inference engine is compiled in at all.
///
/// Taken from ggml's own `GGML_TYPE_COUNT` rather than a list kept here: what
/// a build supports is decided by the library linked into it, and a copy of
/// that list in this repository would be wrong the moment it is updated.
pub fn tensor_type_count() -> Option<u32> {
    #[cfg(feature = "local-inference")]
    {
        // bindgen emits this constant as i32 on Windows and u32 elsewhere.
        #[allow(clippy::unnecessary_cast)]
        Some(llama_cpp_sys_2::GGML_TYPE_COUNT as u32)
    }
    #[cfg(not(feature = "local-inference"))]
    {
        None
    }
}

/// The discrete GPUs a model would be loaded onto: one worker each in
/// per-GPU mode. Empty without local inference or without such GPUs.
pub fn gpu_workers() -> Vec<usize> {
    #[cfg(feature = "local-inference")]
    {
        local_backend::LlamaLocalBackend::gpu_workers()
    }
    #[cfg(not(feature = "local-inference"))]
    {
        Vec::new()
    }
}
#[cfg(test)]
mod tests {
    // reqwest panics when no TLS provider is installed; a test process starts
    // without one, like a headless node that has not opened the mesh yet.
    #[test]
    fn http_clients_build_without_a_provider_installed_first() {
        super::http_client().build().unwrap();
        super::http_client().build().unwrap();
    }
}
