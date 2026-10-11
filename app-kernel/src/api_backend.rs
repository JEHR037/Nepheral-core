// SPDX-License-Identifier: BUSL-1.1
use std::time::Duration;

use async_trait::async_trait;
use nepheral_api::backend::{
    BackendError, BackendEvent, ChatRequest, EventStream, MeshBackend, ModelInfo,
};
use nepheral_api::creds::CredsMode;
use nepheral_api::dispatch::{resolve_model, Mode, TopTierChannel};
use nepheral_api::metering::{charge_on_first_answer, ChargeFn};

use crate::state::AppState;

const STATUS_INTERVAL_MS: u64 = 1_200;
const CHUNK_WORDS: usize = 3;
const CHUNK_DELAY_MS: u64 = 35;

const WAIT_QUIPS: &[&str] = &[
    "pseudonymising identifiers before anything leaves this machine",
    "splitting the encrypted query into shares",
    "drawing a fresh lottery for this message",
    "the elected responder is reconstructing the query",
    "waiting for the responder's answer",
    "verifying the responder's signature",
    "decrypting the reply",
];

#[derive(Clone)]
pub struct AppStateBackend {
    pub state: AppState,
}

impl AppStateBackend {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl MeshBackend for AppStateBackend {
    async fn list_models(&self) -> Vec<ModelInfo> {
        let ai_capable = self.state.ai_engine.read().await.as_ref().is_some();
        // Top-Tier entries describe the network as it is right now, not as it
        // is meant to be: `ready` and the description both come from which
        // authorities have announced the channel on chain.
        let (chat, large, frontier) = {
            let chain = self.state.chain.read().await;
            (
                chain
                    .authority
                    .servers_of(channel_name(TopTierChannel::Chat)),
                chain
                    .authority
                    .servers_of(channel_name(TopTierChannel::Large)),
                chain
                    .authority
                    .servers_of(channel_name(TopTierChannel::Frontier)),
            )
        };
        let top_tier = |id: &str, ch: TopTierChannel, n: usize| ModelInfo {
            id: id.to_string(),
            description: top_tier_description(ch, n),
            ready: n > 0,
        };
        vec![
            ModelInfo {
                id: "nepheral-auto".to_string(),
                description: "Standard tier first; escalates to Top Tier only when the request \
                              carries an open session, and charges only when answered"
                    .to_string(),
                ready: true,
            },
            ModelInfo {
                id: "nepheral-standard".to_string(),
                description: "Mesh Standard tier — per-message lottery over the open mesh"
                    .to_string(),
                ready: true,
            },
            top_tier("nepheral-top-tier-chat", TopTierChannel::Chat, chat),
            top_tier("nepheral-top-tier-large", TopTierChannel::Large, large),
            top_tier(
                "nepheral-top-tier-frontier",
                TopTierChannel::Frontier,
                frontier,
            ),
            ModelInfo {
                id: "qwen3:7b".to_string(),
                description: "Compatibility alias for nepheral-standard".to_string(),
                ready: true,
            },
            ModelInfo {
                id: "qwen3:72b".to_string(),
                description: "Compatibility alias for nepheral-top-tier-chat".to_string(),
                ready: chat > 0,
            },
            ModelInfo {
                id: "nepheral-local".to_string(),
                description: "Local in-process inference (bypasses mesh)".to_string(),
                ready: ai_capable,
            },
        ]
    }

    async fn get_routing(&self) -> CredsMode {
        self.state.api_routing_policy.read().await.clone()
    }

    async fn set_routing(&self, mode: CredsMode) -> Result<(), BackendError> {
        *self.state.api_routing_policy.write().await = mode;
        self.state.save_routing_policy().await;
        Ok(())
    }

    async fn open_session(&self, channel: Option<String>) -> Result<String, BackendError> {
        use core_crypto::pallets::nai::MAX_CHANNEL_NAME_LEN;

        use crate::state::OpenSessionError;

        match self.state.open_session_onchain(channel).await {
            Ok(token) => Ok(hex::encode(token)),
            Err(OpenSessionError::InvalidChannel) => Err(BackendError::BadRequest(format!(
                "invalid channel name (1..={MAX_CHANNEL_NAME_LEN} chars)"
            ))),
            Err(OpenSessionError::InsufficientBalance) => Err(BackendError::BadRequest(
                "insufficient balance to open session — redeem contributions first or buy via LTC"
                    .into(),
            )),
            Err(OpenSessionError::NotIncluded) => Err(BackendError::Internal(
                "session tx was not included in a block within 15s — \
                 is the mesh authority reachable?"
                    .into(),
            )),
            Err(OpenSessionError::Locked) => {
                Err(BackendError::Unauthorized(crate::wallet::LOCKED.into()))
            }
            Err(OpenSessionError::Declined) => Err(BackendError::Unauthorized(
                "the wallet's owner did not confirm the payment".into(),
            )),
        }
    }

    async fn submit(&self, request: ChatRequest) -> Result<EventStream, BackendError> {
        if request.messages.is_empty() {
            return Err(BackendError::BadRequest("no messages".into()));
        }

        let prompt = flatten_messages(&request.messages);

        let Some(mode) = resolve_model(&request.model) else {
            return Err(BackendError::UnknownModel(request.model.clone()));
        };

        let local_only = matches!(
            *self.state.api_routing_policy.read().await,
            CredsMode::LocalOnly
        );
        if local_only && !matches!(mode, Mode::Local | Mode::Auto) {
            return Err(BackendError::BadRequest(
                "routing policy is LocalOnly: this model would leave the machine; use nepheral-local"
                    .into(),
            ));
        }

        match mode {
            Mode::Local => {
                self.run_local(prompt, request.stream, sampling(&request.options))
                    .await
            }
            Mode::Standard => {
                self.run_mesh_passthrough(prompt, None, request.stream, 30_000)
                    .await
            }
            Mode::TopTier(ch) => {
                let channel = channel_name(ch).to_string();
                self.require_served_channel(&channel).await?;
                let token = self
                    .require_active_session(&request.session_token, &channel)
                    .await?;
                let answer = self
                    .run_mesh_passthrough_paid(
                        prompt,
                        Some(channel),
                        Some(token),
                        request.stream,
                        60_000,
                    )
                    .await?;
                Ok(charge_on_first_answer(answer, self.charge_for(token)))
            }
            Mode::Auto => {
                self.run_with_policy(prompt, request.stream, request.session_token.clone())
                    .await
            }
            Mode::Review(template) => self.run_review_mode(template, prompt, request.stream).await,
            Mode::Doc(template) => self.run_doc_mode(template, prompt, request.stream).await,
            Mode::Research(template) => {
                self.run_research_mode(template, prompt, request.stream)
                    .await
            }
            Mode::Dev(template) => self.run_dev_mode(template, prompt, request.stream).await,
        }
    }
}

impl AppStateBackend {
    async fn run_dev_mode(
        &self,
        template: nepheral_api::dispatch::DevTemplate,
        prompt: String,
        stream: bool,
    ) -> Result<EventStream, BackendError> {
        use nepheral_api::modes::dev::{assemble_dev_output, lens_prompt_for, split_dev_input};
        let (instruction, body) = split_instruction_and_body(&prompt);
        let files = split_dev_input(&body);
        if files.is_empty() {
            return Err(BackendError::BadRequest(
                "dev mode: no files in request body".into(),
            ));
        }
        let state = self.state.clone();
        let backend_stream = async_stream::stream! {
            yield BackendEvent::Status(format!(
                "dev: parsed {} file(s), dispatching to mesh in parallel",
                files.len()
            ));

            let mut handles = Vec::with_capacity(files.len());
            for f in files.iter() {
                let p = lens_prompt_for(template, &f.path, &f.content, &instruction);
                let state_for_file = state.clone();
                let idx = f.index;
                let path = f.path.clone();
                handles.push(tokio::spawn(async move {
                    let text = match Self::dispatch_and_wait(
                        &state_for_file,
                        &p,
                        None,
                        45_000,
                    )
                    .await
                    {
                        Ok(t) => t,
                        Err(WaitError::Timeout) => "(file timed out)".to_string(),
                        Err(WaitError::Dispatch(e)) => format!("(dispatch failed: {e})"),
                    };
                    (idx, path, text)
                }));
            }
            let mut results: Vec<(usize, String, String)> = Vec::with_capacity(handles.len());
            for h in handles {
                if let Ok(t) = h.await {
                    yield BackendEvent::Status(format!("dev file '{}' complete", t.1));
                    results.push(t);
                }
            }
            let assembled = assemble_dev_output(template, &results);
            for ev in Self::chunk_response(assembled, stream) {
                yield ev;
                if stream {
                    tokio::time::sleep(Duration::from_millis(CHUNK_DELAY_MS)).await;
                }
            }
            yield BackendEvent::Done;
        };
        Ok(Box::pin(backend_stream))
    }

    async fn run_research_mode(
        &self,
        template: nepheral_api::dispatch::ResearchTemplate,
        prompt: String,
        stream: bool,
    ) -> Result<EventStream, BackendError> {
        use nepheral_api::modes::research::{angles_for, assemble_research_output};
        let angles = angles_for(template);
        let state = self.state.clone();
        let backend_stream = async_stream::stream! {
            yield BackendEvent::Status(format!(
                "research: {} angles in parallel against the same question",
                angles.len()
            ));

            let mut handles = Vec::with_capacity(angles.len());
            for angle in angles.iter() {
                let prompt_for_angle = format!(
                    "{}\n\n=== QUESTION ===\n{}",
                    angle.prompt_prefix, prompt
                );
                let name = angle.name.to_string();
                let state_for_angle = state.clone();
                handles.push(tokio::spawn(async move {
                    let text = match Self::dispatch_and_wait(
                        &state_for_angle,
                        &prompt_for_angle,
                        None,
                        45_000,
                    )
                    .await
                    {
                        Ok(t) => t,
                        Err(WaitError::Timeout) => "(angle timed out)".to_string(),
                        Err(WaitError::Dispatch(e)) => format!("(angle dispatch failed: {e})"),
                    };
                    (name, text)
                }));
            }
            let mut results: Vec<(String, String)> = Vec::with_capacity(handles.len());
            for h in handles {
                if let Ok(pair) = h.await {
                    yield BackendEvent::Status(format!("angle '{}' complete", pair.0));
                    results.push(pair);
                }
            }
            let assembled = assemble_research_output(template, &results);
            for ev in Self::chunk_response(assembled, stream) {
                yield ev;
                if stream {
                    tokio::time::sleep(Duration::from_millis(CHUNK_DELAY_MS)).await;
                }
            }
            yield BackendEvent::Done;
        };
        Ok(Box::pin(backend_stream))
    }

    async fn run_doc_mode(
        &self,
        template: nepheral_api::dispatch::DocTemplate,
        prompt: String,
        stream: bool,
    ) -> Result<EventStream, BackendError> {
        use nepheral_api::modes::doc::{assemble_doc_output, chunk_document, lens_prompt_for};

        let (query, body) = split_query_and_body(&prompt);
        let (chunks, truncated) = chunk_document(&body);
        if chunks.is_empty() {
            return Err(BackendError::BadRequest(
                "doc mode: empty document body".into(),
            ));
        }
        let chunks_total = chunks.len();
        let state = self.state.clone();
        let backend_stream = async_stream::stream! {
            yield BackendEvent::Status(format!(
                "doc: split into {chunks_total} chunks, dispatching in parallel"
            ));
            if truncated {
                yield BackendEvent::Status(format!(
                    "doc: WARNING — input exceeded {} chunks × {} chars; the tail was NOT processed",
                    nepheral_api::modes::doc::MAX_CHUNKS_PER_REQUEST,
                    nepheral_api::modes::doc::TARGET_CHUNK_CHARS,
                ));
            }

            let mut handles = Vec::with_capacity(chunks.len());
            for chunk in chunks.iter() {
                let lens_prompt = lens_prompt_for(template, &chunk.content, query.as_deref());
                let state_for_chunk = state.clone();
                let idx = chunk.index;
                handles.push(tokio::spawn(async move {
                    let text = match Self::dispatch_and_wait(
                        &state_for_chunk,
                        &lens_prompt,
                        None,
                        45_000,
                    )
                    .await
                    {
                        Ok(t) => t,
                        Err(WaitError::Timeout) => "(chunk timed out — no output)".to_string(),
                        Err(WaitError::Dispatch(e)) => format!("(chunk dispatch failed: {e})"),
                    };
                    (idx, text)
                }));
            }
            let mut results: Vec<(usize, String)> = Vec::with_capacity(handles.len());
            for h in handles {
                if let Ok(pair) = h.await {
                    yield BackendEvent::Status(format!("doc chunk {} complete", pair.0));
                    results.push(pair);
                }
            }
            let mut assembled = assemble_doc_output(template, chunks_total, &results);
            if truncated {
                assembled.push_str(
                    "\n\n> Note: the input exceeded the processing cap \
                     (8 chunks × 4,000 chars); content beyond that point was not analyzed.",
                );
            }
            for ev in Self::chunk_response(assembled, stream) {
                yield ev;
                if stream {
                    tokio::time::sleep(Duration::from_millis(CHUNK_DELAY_MS)).await;
                }
            }
            yield BackendEvent::Done;
        };
        Ok(Box::pin(backend_stream))
    }

    async fn require_active_session(
        &self,
        token_hex: &Option<String>,
        channel: &str,
    ) -> Result<[u8; 32], BackendError> {
        let Some(hex_str) = token_hex.as_deref() else {
            return Err(BackendError::Unauthorized(
                "Top Tier requires X-Nepheral-Session-Token (or Authorization: Bearer <hex>); \
                 open one via POST /v1/sessions"
                    .into(),
            ));
        };
        let trimmed = hex_str.trim();
        if trimmed.len() != 64 {
            return Err(BackendError::Unauthorized(
                "session token must be 64 hex chars (32-byte token)".into(),
            ));
        }
        let mut token = [0u8; 32];
        if hex::decode_to_slice(trimmed, &mut token).is_err() {
            return Err(BackendError::Unauthorized(
                "session token is not valid hex".into(),
            ));
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let me = *self.state.my_peer_id.read().await;
        let wallet = self.state.my_account().await;
        let chain = self.state.chain.read().await;
        if chain.nai.session_expired(&token, now_ms) {
            return Err(BackendError::Unauthorized(
                "session token unknown, expired, or out of inferences; \
                 open a new one via POST /v1/sessions"
                    .into(),
            ));
        }
        let Some(session) = chain.nai.session(&token) else {
            return Err(BackendError::Unauthorized(
                "session token unknown; open one via POST /v1/sessions".into(),
            ));
        };
        // Paid by this node, or by the wallet it works for.
        if session.owner != me && session.owner != wallet {
            return Err(BackendError::Unauthorized(
                "session token belongs to a different wallet".into(),
            ));
        }
        // Channel-less sessions (POST /v1/sessions with no body) are
        // universal; channel-bound sessions only unlock their own channel.
        if let Some(bound) = &session.channel {
            if bound != channel {
                return Err(BackendError::Unauthorized(format!(
                    "session is bound to channel '{bound}', not '{channel}'"
                )));
            }
        }
        Ok(token)
    }

    /// Refuses a Top-Tier channel no authority has announced. Such a request
    /// could only time out, and until the spend moved to answer time it also
    /// cost the caller a credit for that timeout.
    async fn require_served_channel(&self, channel: &str) -> Result<(), BackendError> {
        let servers = self.state.chain.read().await.authority.servers_of(channel);
        if servers == 0 {
            return Err(BackendError::BadRequest(format!(
                "no node currently serves the '{channel}' Top-Tier channel — nothing would \
                 answer, so the request is refused before any session use is spent"
            )));
        }
        Ok(())
    }

    /// The spend for one answered Top-Tier request, handed to
    /// `charge_on_first_answer` so it runs only once a real token arrives.
    fn charge_for(&self, token: [u8; 32]) -> ChargeFn {
        let me = self.clone();
        Box::new(move || Box::pin(async move { me.consume_session_inference(token).await }))
    }

    /// Burn one inference from the session by submitting a signed
    /// Nai::UseSession extrinsic. Fire-and-forget: the decrement lands with
    /// the next sealed block (~2 s); require_active_session reads the
    /// chain view, so metering lags at most one block behind. Callers run
    /// this only after the mesh has answered — never on dispatch.
    async fn consume_session_inference(&self, token: [u8; 32]) {
        use core_crypto::pallets::nai::NaiCall;
        self.state
            .sign_and_submit(
                core_crypto::extrinsic::PALLET_NAI,
                &NaiCall::UseSession {
                    session_token: token,
                },
            )
            .await;
    }

    async fn run_review_mode(
        &self,
        template: nepheral_api::dispatch::ReviewTemplate,
        prompt: String,
        stream: bool,
    ) -> Result<EventStream, BackendError> {
        use nepheral_api::modes::review::{assemble_review_output, lenses_for};
        let lenses = lenses_for(template);
        let template_name = template_human_name(template);
        let state = self.state.clone();
        let backend_stream = async_stream::stream! {
            yield BackendEvent::Status(format!(
                "review:{} — dispatching {} lenses in parallel",
                template_name, lenses.len()
            ));

            let mut handles = Vec::with_capacity(lenses.len());
            for lens in lenses.iter() {
                let lens_prompt = format!("{}\n\n=== ARTIFACT TO REVIEW ===\n{}", lens.prompt_prefix, prompt);
                let state_for_lens = state.clone();
                let lens_name = lens.name.to_string();
                handles.push(tokio::spawn(async move {
                    let text = match Self::dispatch_and_wait(
                        &state_for_lens,
                        &lens_prompt,
                        None,
                        45_000,
                    )
                    .await
                    {
                        Ok(t) => t,
                        Err(WaitError::Timeout) => "(lens timed out — no findings collected)".to_string(),
                        Err(WaitError::Dispatch(e)) => format!("(lens dispatch failed: {e})"),
                    };
                    (lens_name, text)
                }));
            }
            let mut results: Vec<(String, String)> = Vec::with_capacity(handles.len());
            for h in handles {
                if let Ok((name, text)) = h.await {
                    yield BackendEvent::Status(format!("review:{template_name} lens '{name}' complete"));
                    results.push((name, text));
                }
            }

            let assembled = assemble_review_output(template_name, &results);
            for ev in Self::chunk_response(assembled, stream) {
                yield ev;
                if stream {
                    tokio::time::sleep(Duration::from_millis(CHUNK_DELAY_MS)).await;
                }
            }
            yield BackendEvent::Done;
        };
        Ok(Box::pin(backend_stream))
    }

    async fn run_with_policy(
        &self,
        prompt: String,
        stream: bool,
        session_token: Option<String>,
    ) -> Result<EventStream, BackendError> {
        let policy = self.state.api_routing_policy.read().await.clone();
        match policy {
            CredsMode::LocalOnly => {
                self.run_local(prompt, stream, ai_engine::Sampling::default())
                    .await
            }
            CredsMode::StandardOnly { total_deadline_ms } => {
                self.run_mesh_passthrough(prompt, None, stream, total_deadline_ms)
                    .await
            }
            // Pinning the policy to Top Tier does not exempt it from paying
            // for Top Tier: the routing mode chooses the channel, the session
            // still pays for the answer.
            CredsMode::TopTierOnly {
                channel,
                total_deadline_ms,
            } => {
                self.require_served_channel(&channel).await?;
                let token = self
                    .require_active_session(&session_token, &channel)
                    .await?;
                let answer = self
                    .run_mesh_passthrough_paid(
                        prompt,
                        Some(channel),
                        Some(token),
                        stream,
                        total_deadline_ms,
                    )
                    .await?;
                Ok(charge_on_first_answer(answer, self.charge_for(token)))
            }
            CredsMode::Cross {
                standard_deadline_ms,
                total_deadline_ms,
                escalation_channel,
            } => {
                self.run_cross_mode(
                    prompt,
                    stream,
                    standard_deadline_ms,
                    total_deadline_ms,
                    escalation_channel,
                    session_token,
                )
                .await
            }
        }
    }

    async fn run_cross_mode(
        &self,
        prompt: String,
        stream: bool,
        standard_deadline_ms: u64,
        total_deadline_ms: u64,
        escalation_channel: String,
        session_token: Option<String>,
    ) -> Result<EventStream, BackendError> {
        let state = self.state.clone();
        let me = self.clone();

        let backend_stream = async_stream::stream! {
            yield BackendEvent::Status("cross-mode: trying Standard tier first".into());
            match Self::dispatch_and_wait(&state, &prompt, None, standard_deadline_ms).await {
                Ok(text) => {
                    for ev in Self::chunk_response(text, stream) {
                        yield ev;
                        if stream {
                            tokio::time::sleep(Duration::from_millis(CHUNK_DELAY_MS)).await;
                        }
                    }
                    yield BackendEvent::Done;
                    return;
                }
                Err(WaitError::Timeout) => {}
                Err(WaitError::Dispatch(e)) => {
                    yield BackendEvent::Error(format!("Standard dispatch failed: {e}"));
                    return;
                }
            }

            // Escalation is a Top-Tier request like any other: it needs a
            // node that serves the channel and a session that pays for the
            // answer. Auto mode used to escalate unmetered — a premium
            // answer, when one came, cost the caller nothing and paid the
            // responder nothing.
            if let Err(e) = me.require_served_channel(&escalation_channel).await {
                yield BackendEvent::Error(format!(
                    "Standard tier timed out after {standard_deadline_ms}ms and escalation is \
                     not possible: {}",
                    backend_error_message(&e)
                ));
                return;
            }
            let token = match me.require_active_session(&session_token, &escalation_channel).await {
                Ok(t) => t,
                Err(e) => {
                    yield BackendEvent::Error(format!(
                        "Standard tier timed out after {standard_deadline_ms}ms; escalating to \
                         Top Tier ({escalation_channel}) needs an open session: {}",
                        backend_error_message(&e)
                    ));
                    return;
                }
            };
            yield BackendEvent::Status(format!(
                "Standard exceeded {}ms — escalating to Top Tier ({})",
                standard_deadline_ms, escalation_channel
            ));

            let remaining = total_deadline_ms.saturating_sub(standard_deadline_ms);
            if remaining == 0 {
                yield BackendEvent::Error("no time budget left for escalation".into());
                return;
            }
            match Self::dispatch_and_wait_paid(
                &state,
                &prompt,
                Some(escalation_channel),
                Some(token),
                remaining,
            )
            .await
            {
                Ok(text) => {
                    // Paid only now, with the answer in hand.
                    if !text.is_empty() {
                        me.consume_session_inference(token).await;
                    }
                    let chunks = Self::chunk_response(text, stream);
                    for ev in chunks {
                        yield ev;
                        if stream {
                            tokio::time::sleep(Duration::from_millis(CHUNK_DELAY_MS)).await;
                        }
                    }
                    yield BackendEvent::Done;
                }
                Err(WaitError::Timeout) => {
                    yield BackendEvent::Error(
                        "Top Tier escalation also timed out".into(),
                    );
                }
                Err(WaitError::Dispatch(e)) => {
                    yield BackendEvent::Error(format!("Top Tier dispatch failed: {e}"));
                }
            }
        };
        Ok(Box::pin(backend_stream))
    }

    async fn run_local(
        &self,
        prompt: String,
        stream: bool,
        sampling: ai_engine::Sampling,
    ) -> Result<EventStream, BackendError> {
        let engine_opt = self.state.ai_engine.read().await.clone();
        let Some(engine) = engine_opt else {
            return Err(BackendError::Internal(
                "local engine not loaded".to_string(),
            ));
        };
        let system = self.state.active_system_prompt().await;
        let backend_stream = async_stream::stream! {
            let mut messages = Vec::new();
            if let Some(sys) = system {
                messages.push(ai_engine::Message {
                    role: ai_engine::MessageRole::System,
                    content: sys,
                    timestamp: 0,
                });
            }
            messages.push(ai_engine::Message {
                role: ai_engine::MessageRole::User,
                content: prompt,
                timestamp: 0,
            });
            match engine.infer_sampled(messages, &sampling).await {
                Ok(r) => {
                    for ev in Self::chunk_response(r.text, stream) {
                        yield ev;
                        if stream {
                            tokio::time::sleep(Duration::from_millis(CHUNK_DELAY_MS)).await;
                        }
                    }
                    yield BackendEvent::Done;
                }
                Err(e) => {
                    yield BackendEvent::Error(format!("local engine: {e}"));
                }
            }
        };
        Ok(Box::pin(backend_stream))
    }

    async fn run_mesh_passthrough(
        &self,
        prompt: String,
        target_channel: Option<String>,
        stream: bool,
        deadline_ms: u64,
    ) -> Result<EventStream, BackendError> {
        self.run_mesh_passthrough_paid(prompt, target_channel, None, stream, deadline_ms)
            .await
    }

    async fn run_mesh_passthrough_paid(
        &self,
        prompt: String,
        target_channel: Option<String>,
        session_token: Option<[u8; 32]>,
        stream: bool,
        deadline_ms: u64,
    ) -> Result<EventStream, BackendError> {
        let state = self.state.clone();
        let prompt_for_stream = prompt.clone();
        let backend_stream = async_stream::stream! {
            yield BackendEvent::Status("submitted to mesh; awaiting responder".into());
            // status quip ticker runs alongside dispatch_and_wait via select
            let started = std::time::Instant::now();
            let quip_idx = (started.elapsed().as_nanos() as usize) % WAIT_QUIPS.len();

            let dispatch_fut = Self::dispatch_and_wait_paid(
                &state,
                &prompt_for_stream,
                target_channel.clone(),
                session_token,
                deadline_ms,
            );
            tokio::pin!(dispatch_fut);

            let mut tick = tokio::time::interval(Duration::from_millis(STATUS_INTERVAL_MS));
            tick.tick().await; // skip immediate tick
            let mut quip_step = 1usize;

            let outcome = loop {
                tokio::select! {
                    res = &mut dispatch_fut => break res,
                    _ = tick.tick() => {
                        let q = WAIT_QUIPS[(quip_idx + quip_step) % WAIT_QUIPS.len()];
                        quip_step += 1;
                        yield BackendEvent::Status(q.to_string());
                    }
                }
            };

            match outcome {
                Ok(text) => {
                    for ev in Self::chunk_response(text, stream) {
                        yield ev;
                        if stream {
                            tokio::time::sleep(Duration::from_millis(CHUNK_DELAY_MS)).await;
                        }
                    }
                    yield BackendEvent::Done;
                }
                Err(WaitError::Timeout) => {
                    yield BackendEvent::Error("mesh response timeout".into());
                }
                Err(WaitError::Dispatch(e)) => {
                    yield BackendEvent::Error(format!("mesh dispatch: {e}"));
                }
            }
        };
        Ok(Box::pin(backend_stream))
    }

    async fn dispatch_and_wait(
        state: &AppState,
        prompt: &str,
        target_channel: Option<String>,
        deadline_ms: u64,
    ) -> Result<String, WaitError> {
        Self::dispatch_and_wait_paid(state, prompt, target_channel, None, deadline_ms).await
    }

    async fn dispatch_and_wait_paid(
        state: &AppState,
        prompt: &str,
        target_channel: Option<String>,
        session_token: Option<[u8; 32]>,
        deadline_ms: u64,
    ) -> Result<String, WaitError> {
        match state
            .ask_mesh(
                prompt,
                target_channel,
                session_token,
                Duration::from_millis(deadline_ms),
            )
            .await
        {
            Ok(text) => Ok(text),
            Err(e) if e.contains("timeout") => Err(WaitError::Timeout),
            Err(e) => Err(WaitError::Dispatch(e)),
        }
    }

    fn chunk_response(text: String, stream: bool) -> Vec<BackendEvent> {
        if !stream {
            return vec![BackendEvent::Token(text)];
        }
        if text.is_empty() {
            return vec![BackendEvent::Token(String::new())];
        }
        let words: Vec<&str> = text.split_inclusive(char::is_whitespace).collect();
        if words.len() <= CHUNK_WORDS {
            return vec![BackendEvent::Token(text)];
        }
        words
            .chunks(CHUNK_WORDS)
            .map(|c| BackendEvent::Token(c.concat()))
            .collect()
    }
}

enum WaitError {
    Timeout,
    Dispatch(String),
}

fn split_instruction_and_body(prompt: &str) -> (String, String) {
    // For dev mode the user puts `instruction: <text>` on the first line then
    // a blank line then the file blob. If no marker, the prompt's first
    // non-empty line is taken as the instruction.
    let trimmed = prompt.trim_start();
    if let Some(rest) = trimmed
        .strip_prefix("instruction:")
        .or_else(|| trimmed.strip_prefix("Instruction:"))
        .or_else(|| trimmed.strip_prefix("INSTRUCTION:"))
    {
        if let Some((line, body)) = rest.split_once('\n') {
            return (line.trim().to_string(), body.trim_start().to_string());
        }
        return (rest.trim().to_string(), String::new());
    }
    if let Some((line, rest)) = prompt.split_once("\n\n") {
        return (line.trim().to_string(), rest.trim_start().to_string());
    }
    if let Some((line, rest)) = prompt.split_once('\n') {
        return (line.trim().to_string(), rest.trim_start().to_string());
    }
    (prompt.trim().to_string(), String::new())
}

fn split_query_and_body(prompt: &str) -> (Option<String>, String) {
    // For doc query-corpus / find-all, the client puts the query on the first
    // line as `query: <text>` (case-insensitive prefix) and the body afterwards.
    // For summarize / extract the entire prompt IS the body and the helper
    // returns query=None.
    let trimmed = prompt.trim_start();
    if let Some(rest) = trimmed
        .strip_prefix("query:")
        .or_else(|| trimmed.strip_prefix("Query:"))
        .or_else(|| trimmed.strip_prefix("QUERY:"))
    {
        if let Some((q_line, body)) = rest.split_once('\n') {
            return (
                Some(q_line.trim().to_string()),
                body.trim_start().to_string(),
            );
        }
        return (Some(rest.trim().to_string()), String::new());
    }
    (None, prompt.to_string())
}

fn template_human_name(t: nepheral_api::dispatch::ReviewTemplate) -> &'static str {
    use nepheral_api::dispatch::ReviewTemplate;
    match t {
        ReviewTemplate::Security => "security",
        ReviewTemplate::Perf => "perf",
        ReviewTemplate::Correctness => "correctness",
        ReviewTemplate::Contract => "contract",
        ReviewTemplate::PrDiff => "pr-diff",
    }
}

fn channel_name(ch: TopTierChannel) -> &'static str {
    match ch {
        TopTierChannel::Chat => "chat",
        TopTierChannel::Large => "large_chat",
        TopTierChannel::Frontier => "frontier",
    }
}

/// What a Top-Tier model entry says about itself, given how many authorities
/// currently serve its channel. Says "attested" of nobody: the whitelist
/// behind `attestation_hash` is manual today, and the description should not
/// promise a hardware guarantee the network does not yet check.
fn top_tier_description(ch: TopTierChannel, servers: usize) -> String {
    let channel = channel_name(ch);
    if servers == 0 {
        return format!(
            "Top Tier '{channel}' channel — no node currently serves it; requests are refused \
             rather than charged"
        );
    }
    format!(
        "Top Tier '{channel}' channel — {servers} node(s) serving; needs an open session, \
         charged only when answered"
    )
}

fn backend_error_message(e: &BackendError) -> String {
    match e {
        BackendError::BadRequest(m)
        | BackendError::Unauthorized(m)
        | BackendError::Internal(m)
        | BackendError::UnknownModel(m) => m.clone(),
        other => format!("{other:?}"),
    }
}

/// What the caller asked of the reply, for a model running here.
fn sampling(o: &nepheral_api::backend::GenerationOptions) -> ai_engine::Sampling {
    ai_engine::Sampling {
        temperature: o.temperature,
        max_tokens: o.max_tokens.map(|n| n as usize),
        stop: o.stop.clone(),
    }
}

fn flatten_messages(messages: &[nepheral_api::backend::ChatMessage]) -> String {
    if messages.len() == 1 {
        return messages[0].content.clone();
    }
    let mut system = Vec::new();
    let mut turns = Vec::new();
    for m in messages {
        if m.role.eq_ignore_ascii_case("system") {
            system.push(m.content.clone());
        } else {
            turns.push(format!("{}: {}", m.role, m.content));
        }
    }
    let mut out = String::new();
    if !system.is_empty() {
        out.push_str("Instructions:\n");
        out.push_str(&system.join("\n"));
        out.push_str("\n\nConversation:\n");
    }
    out.push_str(&turns.join("\n"));
    out
}

/// Checks the local API's token against its hash: the node verifies a
/// request without holding the token, so the API works while the wallet is
/// locked. Each request let in counts as activity.
#[derive(Clone)]
pub struct TokenGate(pub AppState);

impl std::fmt::Debug for TokenGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenGate")
    }
}

impl nepheral_api::server::ApiGate for TokenGate {
    fn accepts(&self, presented: &str) -> bool {
        let Some(stored) = *self.0.api_token_hash.read().expect("api hash lock") else {
            return false;
        };
        let given = crate::wallet::api_token_hash(presented);
        nepheral_api::server::constant_time_eq(&given, &stored)
    }

    fn let_in(&self) {
        self.0.touch();
    }
}
