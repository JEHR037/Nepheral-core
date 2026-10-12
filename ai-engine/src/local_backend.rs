// SPDX-License-Identifier: BUSL-1.1
#[cfg(feature = "local-inference")]
pub use inner::*;

#[cfg(feature = "local-inference")]
mod inner {
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Instant;

    use async_trait::async_trait;
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::model::{LlamaChatMessage, LlamaChatTemplate, LlamaModel};
    use llama_cpp_2::sampling::LlamaSampler;

    use crate::backend::{InferenceBackend, InferenceError};
    use crate::reply_budget::{
        ends_inside_reasoning, fit_messages, reasoning_share, without_reasoning, ReasoningGuard,
        CLOSE_REASONING, REASONING_MIN_TOKENS,
    };

    // llama.cpp's backend context is a process-wide singleton; a second
    // LlamaBackend::init() call errors, so model switches must reuse this.
    static SHARED_BACKEND: OnceLock<Mutex<Option<Arc<LlamaBackend>>>> = OnceLock::new();

    /// Keeps what llama.cpp says about itself.
    ///
    /// llama.cpp reports a refused model as a null pointer, having already
    /// written the actual reason — an unknown tensor quantisation, an
    /// architecture it does not implement, a truncated file — to its own log,
    /// which went to stderr and never reached the application. Routing that
    /// log here means the reason can be quoted back verbatim, so this code
    /// never has to carry its own notion of what the linked llama.cpp
    /// supports. That list changes with every bump of the library, and a copy
    /// of it here would be wrong the moment it was written.
    mod llama_log {
        use std::collections::VecDeque;
        use std::sync::Mutex;

        /// Enough to hold the few lines that precede a failure without
        /// growing without bound over a long-running process.
        const KEEP: usize = 24;

        static RECENT_ERRORS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

        /// Called by llama.cpp on its own threads. It must not unwind into C,
        /// so every step here either succeeds or is skipped.
        unsafe extern "C" fn on_log(
            level: llama_cpp_sys_2::ggml_log_level,
            text: *const std::os::raw::c_char,
            _user_data: *mut std::os::raw::c_void,
        ) {
            if text.is_null() {
                return;
            }
            let Ok(line) = (unsafe { std::ffi::CStr::from_ptr(text) }).to_str() else {
                return;
            };
            let line = line.trim_end();
            if line.is_empty() {
                return;
            }

            // Only a real error. GGML_LOG_LEVEL_CONT (5) sorts above ERROR
            // numerically but means "continuation of the previous line" —
            // llama.cpp emits its loading progress that way, one dot at a
            // time. Treating anything at or above ERROR as an error turned
            // those dots into stored failures, and a stored "." would have
            // become the reason shown to the user.
            if level == llama_cpp_sys_2::GGML_LOG_LEVEL_ERROR {
                tracing::error!(target: "llama_cpp", "{line}");
                if let Ok(mut recent) = RECENT_ERRORS.lock() {
                    if recent.len() == KEEP {
                        recent.pop_front();
                    }
                    recent.push_back(line.to_string());
                }
            } else if level == llama_cpp_sys_2::GGML_LOG_LEVEL_WARN {
                tracing::warn!(target: "llama_cpp", "{line}");
            } else {
                tracing::debug!(target: "llama_cpp", "{line}");
            }
        }

        /// Routes llama.cpp's log here. Safe to call more than once.
        pub fn install() {
            unsafe { llama_cpp_sys_2::llama_log_set(Some(on_log), std::ptr::null_mut()) };
        }

        /// Empties the buffer before an operation whose failure is about to be
        /// explained, so nothing older is mistaken for its cause.
        pub fn clear() {
            if let Ok(mut recent) = RECENT_ERRORS.lock() {
                recent.clear();
            }
        }

        /// Everything llama.cpp reported as an error since the last [`clear`].
        pub fn take() -> Vec<String> {
            RECENT_ERRORS
                .lock()
                .map(|mut r| r.drain(..).collect())
                .unwrap_or_default()
        }
    }

    fn shared_backend() -> Result<Arc<LlamaBackend>, InferenceError> {
        let cell = SHARED_BACKEND.get_or_init(|| Mutex::new(None));
        let mut guard = cell.lock().expect("shared backend lock poisoned");
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }

        // Before anything in llama.cpp can have something to say.
        llama_log::install();

        // gpu-auto builds load backend modules from disk; must happen before
        // LlamaBackend::init() and exactly once (this runs once, here).
        #[cfg(feature = "dynamic-backends")]
        {
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    llama_cpp_2::llama_backend::load_backends_from_path(&dir.join("backends"));
                }
            }
        }

        let backend = Arc::new(LlamaBackend::init()?);
        *guard = Some(backend.clone());
        Ok(backend)
    }

    /// Initializes the shared backend without loading a model, so device
    /// enumeration (hw_detect) has a backend to query.
    pub fn ensure_backend() -> Result<(), InferenceError> {
        shared_backend().map(|_| ())
    }

    /// Flash attention for every context: `NEPHERAL_FLASH_ATTN` set to `on`
    /// or `off` forces it; anything else lets llama.cpp decide per device.
    fn flash_attention() -> llama_cpp_sys_2::llama_flash_attn_type {
        match std::env::var("NEPHERAL_FLASH_ATTN").as_deref() {
            Ok("on") => llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_ENABLED,
            Ok("off") => llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_DISABLED,
            _ => llama_cpp_sys_2::LLAMA_FLASH_ATTN_TYPE_AUTO,
        }
    }

    /// Turns one token's bytes into text. A multi-byte character can be split
    /// across tokens; the decoder keeps the unfinished bytes for the next call.
    fn decode_piece(decoder: &mut encoding_rs::Decoder, bytes: &[u8]) -> String {
        let room = decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(bytes.len() * 3 + 4);
        let mut text = String::with_capacity(room);
        let _ = decoder.decode_to_string(bytes, &mut text, false);
        text
    }

    use crate::types::{FinishReason, Message, MessageRole, ModelConfig, StreamChunk};

    enum ThinkPhase {
        Pre,
        Inside,
        Post,
    }

    struct ThinkParser {
        phase: ThinkPhase,
        buf: String,
    }

    impl ThinkParser {
        fn new() -> Self {
            Self {
                phase: ThinkPhase::Pre,
                buf: String::new(),
            }
        }

        fn floor_char_boundary(s: &str, idx: usize) -> usize {
            let mut i = idx.min(s.len());
            while i > 0 && !s.is_char_boundary(i) {
                i -= 1;
            }
            i
        }

        fn feed<F: FnMut(StreamChunk)>(&mut self, piece: &str, mut emit: F) {
            self.buf.push_str(piece);
            loop {
                match self.phase {
                    ThinkPhase::Pre => {
                        if let Some(start) = self.buf.find("<think>") {
                            let head = self.buf[..start].to_string();
                            if !head.is_empty() {
                                emit(StreamChunk::Response(head));
                            }
                            self.buf.drain(..start + "<think>".len());
                            self.phase = ThinkPhase::Inside;
                            continue;
                        }
                        let safe = self.buf.len().saturating_sub("<think>".len() - 1);
                        let safe = Self::floor_char_boundary(&self.buf, safe);
                        if safe > 0 {
                            let chunk: String = self.buf.drain(..safe).collect();
                            emit(StreamChunk::Response(chunk));
                        }
                        break;
                    }
                    ThinkPhase::Inside => {
                        if let Some(end) = self.buf.find("</think>") {
                            let head = self.buf[..end].to_string();
                            if !head.is_empty() {
                                emit(StreamChunk::Thinking(head));
                            }
                            self.buf.drain(..end + "</think>".len());
                            self.phase = ThinkPhase::Post;
                            continue;
                        }
                        let safe = self.buf.len().saturating_sub("</think>".len() - 1);
                        let safe = Self::floor_char_boundary(&self.buf, safe);
                        if safe > 0 {
                            let chunk: String = self.buf.drain(..safe).collect();
                            emit(StreamChunk::Thinking(chunk));
                        }
                        break;
                    }
                    ThinkPhase::Post => {
                        if !self.buf.is_empty() {
                            let chunk = std::mem::take(&mut self.buf);
                            emit(StreamChunk::Response(chunk));
                        }
                        break;
                    }
                }
            }
        }

        fn flush<F: FnMut(StreamChunk)>(&mut self, mut emit: F) {
            if self.buf.is_empty() {
                return;
            }
            let leftover = std::mem::take(&mut self.buf);
            match self.phase {
                ThinkPhase::Pre | ThinkPhase::Post => emit(StreamChunk::Response(leftover)),
                ThinkPhase::Inside => emit(StreamChunk::Thinking(leftover)),
            }
        }
    }

    /// Shown in place of a reply that never got past its own reasoning.
    ///
    /// A model that reasons before answering spends tokens doing it, and the
    /// reasoning is stripped from what the user sees. When the limit runs out
    /// before it finishes, everything generated is discarded and the reply
    /// arrives empty — indistinguishable, from the outside, from a model that
    /// had nothing to say or a system that had broken.
    pub const CUT_OFF_WHILE_REASONING: &str = concat!(
        "The reply ran out of room while the model was still reasoning, so there was ",
        "nothing left to show. Raise the reply limit in AI Settings, or ask something narrower."
    );

    /// What a reply cut short by a model switch reports, in place of text.
    pub const STOPPED_BY_SWITCH: &str = "the reply was stopped because the model was switched";

    pub struct LlamaLocalBackend {
        model: Arc<LlamaModel>,
        backend: Arc<LlamaBackend>,
        /// Set when this engine is being replaced. The generation loop reads
        /// it at every token, which is the only place a running reply can be
        /// stopped: llama.cpp's own abort hook is not exposed by the bindings,
        /// so a prompt already being evaluated runs to the end of that step.
        cancelled: Arc<AtomicBool>,
        /// The prompt format baked into the GGUF. Every model family has its
        /// own; feeding one a different family's tags produces text that runs
        /// past where the model meant to stop, because the stop tag it emits
        /// is not the one the prompt led it to expect.
        template: Option<LlamaChatTemplate>,
        /// The template knows a reasoning block (`<think>`), so this model
        /// reasons before it answers unless told not to.
        reasons: bool,
    }

    /// The context size and reply budget a reply runs with.
    struct Budgets {
        n_ctx: usize,
        n_ubatch: u32,
        max_tokens: usize,
    }

    impl LlamaLocalBackend {
        /// Where the model's layers go: see `device_choice`. Must run after
        /// the backend is up, since a `gpu-auto` build only learns its
        /// devices by loading the backend modules.
        /// The discrete GPUs the usual choice would load onto: a rig's
        /// workers, one per card, in per-GPU mode.
        pub fn gpu_workers() -> Vec<usize> {
            use crate::device_choice::{choose, Placement};
            if shared_backend().is_err() {
                return Vec::new();
            }
            match choose(&Self::devices(), false) {
                Placement::Only(indexes) => indexes,
                _ => Vec::new(),
            }
        }

        fn devices() -> Vec<crate::device_choice::Device> {
            use crate::device_choice::{Device, DeviceKind};
            use llama_cpp_2::{list_llama_ggml_backend_devices, LlamaBackendDeviceType};
            list_llama_ggml_backend_devices()
                .into_iter()
                .map(|d| Device {
                    index: d.index,
                    kind: match d.device_type {
                        LlamaBackendDeviceType::Gpu => DeviceKind::Discrete,
                        LlamaBackendDeviceType::IntegratedGpu => DeviceKind::Integrated,
                        _ => DeviceKind::Other,
                    },
                    backend: d.backend,
                })
                .collect()
        }

        fn model_params(only: Option<&[usize]>) -> LlamaModelParams {
            use crate::device_choice::{choose, Device, DeviceKind, Placement};
            use llama_cpp_2::{list_llama_ggml_backend_devices, LlamaBackendDeviceType};

            if let Some(only) = only {
                tracing::info!(devices = ?only, "Model placement chosen by the caller");
                return LlamaModelParams::default()
                    .with_devices(only)
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "Chosen devices refused - letting llama.cpp pick");
                        LlamaModelParams::default()
                    });
            }
            let devices: Vec<Device> = list_llama_ggml_backend_devices()
                .into_iter()
                .map(|d| Device {
                    index: d.index,
                    kind: match d.device_type {
                        LlamaBackendDeviceType::Gpu => DeviceKind::Discrete,
                        LlamaBackendDeviceType::IntegratedGpu => DeviceKind::Integrated,
                        _ => DeviceKind::Other,
                    },
                    backend: d.backend,
                })
                .collect();
            let allow_integrated =
                std::env::var("NEPHERAL_USE_IGPU").is_ok_and(|v| v.trim() == "1");
            let placement = choose(&devices, allow_integrated);
            tracing::info!(?placement, devices = devices.len(), "Model placement");
            match placement {
                Placement::AllDevices => LlamaModelParams::default(),
                Placement::Only(indexes) => LlamaModelParams::default()
                    .with_devices(&indexes)
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "Chosen devices refused - letting llama.cpp pick");
                        LlamaModelParams::default()
                    }),
                // No layers on a GPU is not enough: with a GPU in the model's
                // device list llama.cpp still sends the big matrix products
                // there. An empty list keeps everything on the CPU.
                Placement::CpuOnly => LlamaModelParams::default()
                    .with_n_gpu_layers(0)
                    .with_devices(&[])
                    .unwrap_or_else(|_| LlamaModelParams::default().with_n_gpu_layers(0)),
            }
        }

        pub fn load(model_path: &str) -> Result<Self, InferenceError> {
            Self::load_on(model_path, None)
        }

        /// Loaded on exactly `devices` when given.
        pub fn load_on(
            model_path: &str,
            devices: Option<&[usize]>,
        ) -> Result<Self, InferenceError> {
            let backend = shared_backend()?;
            let params = Self::model_params(devices);
            llama_log::clear();
            let model = LlamaModel::load_from_file(&backend, Path::new(model_path), &params)
                .map_err(|e| Self::explain_load_failure(model_path, e))?;

            let template = model.chat_template(None).ok();
            let reasons = template
                .as_ref()
                .and_then(|t| t.to_str().ok())
                .is_some_and(|t| t.contains("<think>"));
            if template.is_none() {
                tracing::warn!(
                    path = %model_path,
                    "GGUF carries no chat template — falling back to ChatML, which is wrong for \
                     any model that doesn't use it"
                );
            }

            tracing::info!(
                path = %model_path,
                vocab_size = %model.n_vocab(),
                has_chat_template = template.is_some(),
                reasons,
                "Local GGUF model loaded"
            );

            Ok(Self {
                model: Arc::new(model),
                backend,
                cancelled: Arc::new(AtomicBool::new(false)),
                template,
                reasons,
            })
        }

        /// Turns llama.cpp's bare "null result" into something actionable.
        ///
        /// The loader returns a null pointer with no reason attached, so the
        /// reason is taken from llama.cpp's own log and quoted. What this
        /// build supports is decided by the llama.cpp compiled into it, and
        /// that is the only thing entitled to say a model is unsupported.
        fn explain_load_failure(
            model_path: &str,
            original: impl std::fmt::Display,
        ) -> InferenceError {
            let reported = llama_log::take();
            let architecture =
                crate::gguf_probe::probe(Path::new(model_path)).and_then(|i| i.architecture);

            tracing::error!(
                path = %model_path,
                architecture = architecture.as_deref().unwrap_or("unknown"),
                "Model rejected by llama.cpp"
            );

            const LIMIT: &str = concat!(
                "Nepheral runs what the llama.cpp built into this release understands; ",
                "a model needing a newer or patched llama.cpp cannot load until that ",
                "library is updated."
            );

            // llama.cpp reports a failure as a cascade: the root cause, then
            // each layer above it restating that the layer below failed. Only
            // the first line carries information.
            //
            // When it spoke, that line is the whole answer, and the declared
            // architecture adds nothing - naming it alongside reads as an
            // accusation, and it is usually innocent. The common case is an
            // unsupported quantisation of a perfectly supported architecture,
            // where another quantisation of the same model loads fine.
            if let Some(reason) = reported.first() {
                return format!("{reason}. {LIMIT}").into();
            }

            match architecture {
                Some(arch) => {
                    format!("{original}. The file declares architecture '{arch}'. {LIMIT}").into()
                }
                None => format!(
                    "{original}. Its header could not be read either, so the file is most likely incomplete or corrupt. Delete it and download it again."
                )
                .into(),
            }
        }

        pub fn neutralize_control_markup(content: &str) -> String {
            const MARKERS: [&str; 8] = [
                "<start_of_turn>",
                "<end_of_turn>",
                "[INST]",
                "[/INST]",
                "<s>",
                "</s>",
                "<<SYS>>",
                "<</SYS>>",
            ];
            let mut out = content
                .replace('\0', "")
                .replace("<|", "<\u{200B}|")
                .replace("|>", "|\u{200B}>");
            for marker in MARKERS {
                if out.contains(marker) {
                    let (head, tail) = marker.split_at(1);
                    out = out.replace(marker, &format!("{head}\u{200B}{tail}"));
                }
            }
            out
        }

        fn role_name(role: &MessageRole) -> &'static str {
            match role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            }
        }

        /// Renders the conversation in whatever format this model was trained
        /// on, taken from the GGUF itself.
        ///
        /// This used to emit ChatML unconditionally. A model that uses
        /// anything else — Phi-3's `<|user|>`/`<|end|>`, Llama's header tags —
        /// then never saw its own turn markers, so it kept generating past the
        /// end of its answer and looped, which is what the repeating garbage
        /// replies were.
        fn format_prompt(&self, messages: &[Message]) -> String {
            if let Some(template) = &self.template {
                let chat: Vec<LlamaChatMessage> = messages
                    .iter()
                    .filter_map(|m| {
                        LlamaChatMessage::new(
                            Self::role_name(&m.role).to_string(),
                            Self::neutralize_control_markup(&m.content),
                        )
                        .ok()
                    })
                    .collect();
                match self.model.apply_chat_template(template, &chat, true) {
                    Ok(prompt) => return prompt,
                    Err(e) => tracing::warn!(
                        error = %e,
                        "Model's chat template failed to render — falling back to ChatML"
                    ),
                }
            }
            Self::format_prompt_chatml(messages)
        }

        /// The prompt for `messages`, fitted to the context with the real
        /// tokenizer and with reasoning switched off unless it is allowed.
        fn prepare_prompt(&self, messages: &[Message], config: &ModelConfig) -> String {
            let b = Self::budgets(config, self.reasons);
            let max_prompt = b.n_ctx.saturating_sub(b.max_tokens).saturating_sub(1);
            let render = |ms: &[Message]| {
                let p = self.format_prompt(ms);
                if self.reasons && !config.allow_reasoning {
                    without_reasoning(&p)
                } else {
                    p
                }
            };
            let count = |p: &str| self.model.vocab().tokenize(p.as_bytes(), false, true).len();
            let fitted = fit_messages(messages, max_prompt, render, count);
            if fitted.dropped > 0 {
                tracing::info!(
                    dropped = fitted.dropped,
                    max_prompt,
                    "Older turns left out so the conversation fits this model's context"
                );
            }
            fitted.prompt
        }

        /// Context, batch and reply budget for one reply.
        ///
        /// The reply reservation must never swallow the whole context: on a
        /// small machine n_ctx floors at N_CTX_MIN while max_tokens keeps its
        /// (much larger) configured default, and n_ctx - max_tokens saturates
        /// to 0, which silently drained every prompt token.
        fn budgets(config: &ModelConfig, reasons: bool) -> Budgets {
            let n_ctx = if config.context_window == 0 {
                Self::FALLBACK_N_CTX
            } else {
                config
                    .context_window
                    .clamp(Self::N_CTX_MIN, Self::N_CTX_MAX)
            };
            let n_ubatch = if config.n_ubatch == 0 {
                Self::FALLBACK_N_UBATCH
            } else {
                config
                    .n_ubatch
                    .clamp(Self::N_UBATCH_MIN, Self::N_UBATCH_MAX)
            };
            // A model allowed to reason needs room to, or its answer never
            // comes: 512 tokens is a short answer but a cut-off thought.
            let wanted = if reasons && config.allow_reasoning {
                config.max_tokens.max(REASONING_MIN_TOKENS)
            } else {
                config.max_tokens
            };
            let max_tokens = wanted.min(Self::MAX_NEW_TOKENS_CAP).min(n_ctx / 2).max(1);
            Budgets {
                n_ctx,
                n_ubatch,
                max_tokens,
            }
        }

        fn format_prompt_chatml(messages: &[Message]) -> String {
            let mut prompt = String::new();
            for msg in messages {
                prompt.push_str(&format!(
                    "<|im_start|>{}\n{}<|im_end|>\n",
                    Self::role_name(&msg.role),
                    Self::neutralize_control_markup(&msg.content)
                ));
            }
            prompt.push_str("<|im_start|>assistant\n");
            prompt
        }

        fn strip_thinking(output: &str) -> String {
            match output.rfind("</think>") {
                Some(end) => output[end + "</think>".len()..].trim().to_string(),
                None => {
                    if output.trim_start().starts_with("<think>") {
                        String::new()
                    } else {
                        output.trim().to_string()
                    }
                }
            }
        }

        const FALLBACK_N_CTX: usize = 4096;
        const N_CTX_MAX: usize = 16384;
        const N_CTX_MIN: usize = 1024;
        const FALLBACK_N_UBATCH: u32 = 256;
        const N_UBATCH_MIN: u32 = 64;
        const N_UBATCH_MAX: u32 = 1024;
        const MAX_NEW_TOKENS_CAP: usize = 4096;

        fn run_inference(
            model: &LlamaModel,
            backend: &LlamaBackend,
            prompt: &str,
            config: &ModelConfig,
            reasons: bool,
            cancelled: &AtomicBool,
        ) -> Result<(String, usize), InferenceError> {
            Self::run_inference_streaming(
                model,
                backend,
                prompt,
                config,
                reasons,
                cancelled,
                |_| {},
            )
            .map(|(text, _, generated)| (text, generated))
        }

        fn run_inference_streaming<F>(
            model: &LlamaModel,
            backend: &LlamaBackend,
            prompt: &str,
            config: &ModelConfig,
            reasons: bool,
            cancelled: &AtomicBool,
            mut on_piece: F,
        ) -> Result<(String, FinishReason, usize), InferenceError>
        where
            F: FnMut(&str),
        {
            let vocab = model.vocab();
            let mut tokens = vocab.tokenize(prompt.as_bytes(), false, true);

            if tokens.is_empty() {
                return Err("empty prompt after tokenization".into());
            }

            let Budgets {
                n_ctx,
                n_ubatch,
                max_tokens,
            } = Self::budgets(config, reasons);

            let max_prompt = n_ctx.saturating_sub(max_tokens).saturating_sub(1);
            if max_prompt == 0 {
                return Err("context window too small to hold any prompt".into());
            }
            // Only reached when the system prompt and the latest message
            // alone overflow: older turns were already left out whole.
            if tokens.len() > max_prompt {
                tracing::warn!(
                    original = tokens.len(),
                    kept = max_prompt,
                    "Prompt exceeds context window even without history — truncating oldest tokens"
                );
                tokens.drain(..tokens.len() - max_prompt);
            }

            let actual_max = max_tokens.min(n_ctx.saturating_sub(tokens.len()).saturating_sub(1));
            if actual_max == 0 {
                return Err("no context space remaining for generation".into());
            }

            tracing::info!(
                prompt_tokens = tokens.len(),
                max_new = actual_max,
                n_ctx,
                n_ubatch,
                "Starting local inference (streaming)"
            );

            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(std::num::NonZeroU32::new(n_ctx as u32))
                .with_n_batch(n_ctx as u32)
                .with_n_ubatch(n_ubatch)
                .with_flash_attention_policy(flash_attention());
            let mut ctx = model.new_context(backend, ctx_params)?;

            // Everything that touches the context runs inside this closure, so
            // that the KV cache is zeroed on every way out of it — each `?`
            // included, and a cancellation — not only when a reply finishes.
            // The cache holds the conversation in another form; freeing it
            // returns the memory to the allocator with that content still in
            // it, which is not the same as erasing it.
            let result = (|| -> Result<(String, FinishReason, usize), InferenceError> {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(STOPPED_BY_SWITCH.into());
                }

                let mut batch = LlamaBatch::new(tokens.len() + actual_max, 1);

                let n_prompt = tokens.len();
                for (i, &token) in tokens.iter().enumerate() {
                    let is_last = i == n_prompt - 1;
                    batch.add(token, i as i32, &[0], is_last)?;
                }
                ctx.decode(&mut batch)?;

                // A sampler chain ends with whichever sampler picks the token, and
                // that choice overrides everything before it. Ending with greedy()
                // after temp/top_k/top_p made those three dead configuration and
                // decoding always argmax — which is exactly how a model falls into
                // repeating the same phrase forever. dist() draws from the
                // distribution the earlier stages shaped.
                //
                // Repetition penalty is 1.1 over a 256-token window, not 1.8 over
                // 64: a window shorter than the loop it is meant to break sees no
                // repetition to penalise, and a penalty that strong distorts
                // ordinary text that happens to reuse a word.
                const PENALTY_WINDOW: i32 = 256;
                const PENALTY_REPEAT: f32 = 1.1;
                // llama.cpp asks the penalty sampler for the vocabulary size
                // directly rather than reading it from the context.
                let n_vocab = model.n_vocab();

                let mut sampler = if config.temperature < 0.01 {
                    LlamaSampler::chain_simple([
                        LlamaSampler::penalties(n_vocab, PENALTY_WINDOW, PENALTY_REPEAT, 0.0, 0.0),
                        LlamaSampler::greedy(),
                    ])
                } else {
                    LlamaSampler::chain_simple([
                        LlamaSampler::penalties(n_vocab, PENALTY_WINDOW, PENALTY_REPEAT, 0.0, 0.0),
                        LlamaSampler::top_k(40),
                        LlamaSampler::top_p(0.9, 1),
                        LlamaSampler::temp(config.temperature),
                        LlamaSampler::dist(rand::random()),
                    ])
                };

                let mut output = String::new();
                let mut n_cur = n_prompt as i32;
                let max_pos = n_cur + actual_max as i32;
                let mut decoder = encoding_rs::UTF_8.new_decoder();
                // Counted for the service receipt: reasoning is compute spent
                // too, and a reply that reasoned for minutes used to report 0.
                let mut generated: usize = 0;
                let mut guard = reasons.then(|| {
                    ReasoningGuard::new(ends_inside_reasoning(prompt), reasoning_share(actual_max))
                });

                let finish: FinishReason = loop {
                    if cancelled.load(Ordering::Relaxed) {
                        return Err(STOPPED_BY_SWITCH.into());
                    }
                    let new_token = sampler.sample(&ctx, batch.n_tokens().saturating_sub(1));
                    sampler.accept(new_token);

                    if vocab.is_eog(new_token) {
                        break FinishReason::Eog;
                    }
                    if n_cur >= max_pos {
                        break FinishReason::MaxTokens;
                    }

                    let piece =
                        decode_piece(&mut decoder, &vocab.token_to_piece(new_token, true, None));

                    // Belt and braces behind is_eog: a GGUF whose metadata
                    // omits its stop token would otherwise generate to the cap.
                    if piece.contains("<|im_end|>")
                        || piece.contains("<|endoftext|>")
                        || piece.contains("<|end|>")
                        || piece.contains("<|eot_id|>")
                    {
                        break FinishReason::StopMarker;
                    }

                    on_piece(&piece);
                    output.push_str(&piece);
                    if let Some(cut) = config
                        .stop
                        .iter()
                        .filter(|s| !s.is_empty())
                        .filter_map(|s| output.find(s.as_str()))
                        .min()
                    {
                        output.truncate(cut);
                        break FinishReason::StopMarker;
                    }

                    batch.clear();
                    batch.add(new_token, n_cur, &[0], true)?;
                    ctx.decode(&mut batch)?;

                    n_cur += 1;
                    generated += 1;

                    // A reasoning block that has used its share is closed for
                    // the model, so the rest of the budget goes to the answer.
                    if guard.as_mut().is_some_and(|g| g.on_piece(&piece)) {
                        let close = vocab.tokenize(CLOSE_REASONING.as_bytes(), false, true);
                        if n_cur + (close.len() as i32) < max_pos {
                            for token in close {
                                sampler.accept(token);
                                batch.clear();
                                batch.add(token, n_cur, &[0], true)?;
                                ctx.decode(&mut batch)?;
                                n_cur += 1;
                                generated += 1;
                            }
                            on_piece(CLOSE_REASONING);
                            output.push_str(CLOSE_REASONING);
                            tracing::info!(
                                reasoning_tokens = generated,
                                "Reasoning closed at its share of the budget"
                            );
                        }
                        if let Some(g) = guard.as_mut() {
                            g.closed();
                        }
                    }
                };

                Ok((output.trim().to_string(), finish, generated))
            })();

            // llama_memory_clear(mem, data = true): ggml writes zeros over
            // every KV buffer, rather than only marking the cells free.
            ctx.clear_kv_cache();
            result
        }
    }

    #[async_trait]
    impl InferenceBackend for LlamaLocalBackend {
        async fn health_check(&self) -> bool {
            true // model is already loaded; if we're here, it's healthy
        }

        async fn chat(
            &self,
            messages: &[Message],
            config: &ModelConfig,
        ) -> Result<(String, usize, u64), InferenceError> {
            let model = Arc::clone(&self.model);
            let backend = Arc::clone(&self.backend);
            let cancelled = Arc::clone(&self.cancelled);
            let prompt = self.prepare_prompt(messages, config);
            let config_for_blocking = config.clone();
            let reasons = self.reasons;

            let start = Instant::now();

            let (raw, tokens) = tokio::task::spawn_blocking(move || {
                Self::run_inference(
                    &model,
                    &backend,
                    &prompt,
                    &config_for_blocking,
                    reasons,
                    &cancelled,
                )
            })
            .await??;

            let mut text = Self::strip_thinking(&raw);
            let elapsed = start.elapsed().as_millis() as u64;

            // Generated something, showed nothing: the reasoning block used
            // the whole budget. Saying so beats an empty bubble.
            if text.is_empty() && !raw.trim().is_empty() {
                tracing::warn!(
                    generated_chars = raw.len(),
                    time_ms = %elapsed,
                    "Reply cut off inside the reasoning block"
                );
                text = CUT_OFF_WHILE_REASONING.to_string();
            }

            tracing::info!(tokens = %tokens, time_ms = %elapsed, "Local inference complete");

            Ok((text, tokens, elapsed))
        }

        async fn chat_stream(
            &self,
            messages: &[Message],
            config: &ModelConfig,
        ) -> Result<tokio::sync::mpsc::UnboundedReceiver<StreamChunk>, InferenceError> {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

            let model = Arc::clone(&self.model);
            let backend = Arc::clone(&self.backend);
            let cancelled = Arc::clone(&self.cancelled);
            let prompt = self.prepare_prompt(messages, config);
            let config_for_blocking = config.clone();
            let reasons = self.reasons;

            tokio::task::spawn_blocking(move || {
                let start = Instant::now();
                let mut parser = ThinkParser::new();
                let mut visible_chars: usize = 0;

                let tx_for_pieces = tx.clone();
                let result = Self::run_inference_streaming(
                    &model,
                    &backend,
                    &prompt,
                    &config_for_blocking,
                    reasons,
                    &cancelled,
                    |piece| {
                        parser.feed(piece, |chunk| {
                            if let StreamChunk::Response(ref s) = chunk {
                                visible_chars += s.len();
                            }
                            let _ = tx_for_pieces.send(chunk);
                        });
                    },
                );

                parser.flush(|chunk| {
                    if let StreamChunk::Response(ref s) = chunk {
                        visible_chars += s.len();
                    }
                    let _ = tx.send(chunk);
                });

                let elapsed = start.elapsed().as_millis() as u64;
                match result {
                    Ok((raw, finish_reason, generated)) => {
                        // Nothing visible was ever emitted, yet tokens were
                        // generated: the reasoning block consumed the budget.
                        if visible_chars == 0 && !raw.trim().is_empty() {
                            tracing::warn!(
                                generated_chars = raw.len(),
                                time_ms = elapsed,
                                "Reply cut off inside the reasoning block"
                            );
                            let _ =
                                tx.send(StreamChunk::Response(CUT_OFF_WHILE_REASONING.to_string()));
                        }
                        let tokens = generated;
                        tracing::info!(
                            tokens,
                            time_ms = elapsed,
                            ?finish_reason,
                            "Local inference stream complete"
                        );
                        let _ = tx.send(StreamChunk::Done {
                            tokens_used: tokens,
                            processing_time_ms: elapsed,
                            finish_reason,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Local inference stream errored");
                        let _ = tx.send(StreamChunk::Error(e.to_string()));
                    }
                }
            });

            Ok(rx)
        }

        fn backend_name(&self) -> &str {
            "local-gguf"
        }

        fn cancel(&self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
}
