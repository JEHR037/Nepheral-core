// SPDX-License-Identifier: BUSL-1.1
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use nepheral_api::backend::{
    BackendError, BackendEvent, ChatRequest, EventStream, MeshBackend, ModelInfo,
};
use nepheral_api::server::{router, ApiGate, ApiSecurity};
use tower::util::ServiceExt;

fn test_router() -> axum::Router {
    router(
        Arc::new(EchoBackend),
        ApiSecurity {
            allow_unauthenticated: true,
            ..Default::default()
        },
    )
}

#[derive(Default)]
struct EchoBackend;

#[async_trait]
impl MeshBackend for EchoBackend {
    async fn list_models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: "nepheral-standard".into(),
            description: "echo".into(),
            ready: true,
        }]
    }

    async fn submit(&self, request: ChatRequest) -> Result<EventStream, BackendError> {
        let last = request
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let stream = async_stream::stream! {
            yield BackendEvent::Token(format!("echo: {last}"));
            yield BackendEvent::Done;
        };
        Ok(Box::pin(stream))
    }
}

#[tokio::test]
async fn get_v1_models_returns_list() {
    let app = test_router();
    let req = Request::builder()
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["object"], "list");
    let data = v["data"].as_array().expect("data array");
    assert!(!data.is_empty());
    assert_eq!(data[0]["id"], "nepheral-standard");
    assert_eq!(data[0]["owned_by"], "nepheral");
}

#[tokio::test]
async fn post_chat_completions_non_stream_returns_aggregated() {
    let app = test_router();
    let body = serde_json::json!({
        "model": "nepheral-standard",
        "messages": [{"role": "user", "content": "hello"}]
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["object"], "chat.completion");
    assert_eq!(v["choices"][0]["message"]["role"], "assistant");
    assert_eq!(v["choices"][0]["message"]["content"], "echo: hello");
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
}

#[tokio::test]
async fn post_chat_completions_streaming_emits_sse() {
    let app = test_router();
    let body = serde_json::json!({
        "model": "nepheral-standard",
        "messages": [{"role": "user", "content": "ping"}],
        "stream": true
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").starts_with("text/event-stream"))
        .unwrap_or(false));
    let mut body_stream = res.into_body().into_data_stream();
    let mut full = Vec::new();
    while let Some(chunk) = body_stream.next().await {
        full.extend_from_slice(&chunk.unwrap());
    }
    let text = String::from_utf8(full).unwrap();
    assert!(text.contains("echo: ping"), "stream missing token: {text}");
    assert!(
        text.contains("[DONE]"),
        "stream missing done marker: {text}"
    );
    assert!(
        text.contains("\"role\":\"assistant\""),
        "stream missing role: {text}"
    );
}

#[tokio::test]
async fn post_chat_completions_rejects_empty_messages() {
    let app = test_router();
    let body = serde_json::json!({
        "model": "nepheral-standard",
        "messages": []
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_v1_routing_returns_default_cross() {
    let app = test_router();
    let req = Request::builder()
        .uri("/v1/routing")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["mode"], "cross");
    assert!(v["standard_deadline_ms"].is_u64());
    assert!(v["total_deadline_ms"].is_u64());
}

#[tokio::test]
async fn put_v1_routing_returns_not_implemented_on_default_backend() {
    // The default impl rejects writes; AppStateBackend overrides this.
    let app = test_router();
    let body = serde_json::json!({"mode":"local_only"});
    let req = Request::builder()
        .method("PUT")
        .uri("/v1/routing")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn post_v1_sessions_returns_not_implemented_on_default_backend() {
    // Default MeshBackend impl rejects; AppStateBackend overrides.
    let app = test_router();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/sessions")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"channel":"chat"}"#))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn post_v1_sessions_accepts_empty_body() {
    let app = test_router();
    let req = Request::builder()
        .method("POST")
        .uri("/v1/sessions")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    // Default backend returns 501; what matters is empty body doesn't 400.
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn ollama_api_chat_non_stream_returns_message() {
    let app = test_router();
    let body = serde_json::json!({
        "model": "nepheral-standard",
        "messages": [{"role": "user", "content": "ola"}],
        "stream": false
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["message"]["role"], "assistant");
    assert_eq!(v["message"]["content"], "echo: ola");
    assert_eq!(v["done"], true);
}

#[tokio::test]
async fn ollama_api_chat_stream_default_emits_ndjson_chunks() {
    let app = test_router();
    let body = serde_json::json!({
        "model": "nepheral-standard",
        "messages": [{"role": "user", "content": "hi"}]
        // stream omitted → defaults to true per Ollama spec
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("")),
        Some("application/x-ndjson")
    );
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "expected content chunk + done chunk: {text}"
    );
    let chunk: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(chunk["done"], false);
    assert_eq!(chunk["message"]["content"], "echo: hi");
    let done: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(done["done"], true);
    assert_eq!(done["done_reason"], "stop");
}

#[tokio::test]
async fn ollama_api_generate_returns_response_field() {
    let app = test_router();
    let body = serde_json::json!({
        "model": "nepheral-standard",
        "prompt": "Hello",
        "stream": false
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/generate")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["response"], "echo: Hello");
    assert_eq!(v["done"], true);
}

#[tokio::test]
async fn ollama_api_tags_lists_models() {
    let app = test_router();
    let req = Request::builder()
        .uri("/api/tags")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let models = v["models"].as_array().expect("models array");
    assert!(!models.is_empty());
    assert_eq!(models[0]["name"], "nepheral-standard");
    assert_eq!(models[0]["details"]["format"], "nepheral");
}

#[tokio::test]
async fn ollama_api_version_returns_nepheral_version() {
    let app = test_router();
    let req = Request::builder()
        .uri("/api/version")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["version"].as_str().unwrap().contains("nepheral"));
}

#[tokio::test]
async fn healthz_returns_ok() {
    let app = test_router();
    let req = Request::builder()
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

fn secured_router() -> axum::Router {
    router(
        Arc::new(EchoBackend),
        ApiSecurity {
            auth_token: Some("sekrit".into()),
            allowed_origins: vec![],
            allow_unauthenticated: false,
            gate: None,
        },
    )
}

#[tokio::test]
async fn auth_token_missing_returns_401() {
    let app = secured_router();
    let req = Request::builder()
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_token_wrong_returns_401() {
    let app = secured_router();
    let req = Request::builder()
        .uri("/v1/models")
        .header("authorization", "Bearer wrong")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_token_bearer_accepted() {
    let app = secured_router();
    let req = Request::builder()
        .uri("/v1/models")
        .header("authorization", "Bearer sekrit")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_token_custom_header_accepted() {
    let app = secured_router();
    let req = Request::builder()
        .uri("/v1/models")
        .header("x-nepheral-api-token", "sekrit")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_exempts_healthz_and_version() {
    for path in ["/healthz", "/api/version"] {
        let app = secured_router();
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path} must stay open");
    }
}

#[tokio::test]
async fn a_router_without_a_token_fails_closed() {
    let app = router(Arc::new(EchoBackend), ApiSecurity::default());
    let req = Request::builder()
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_rebound_host_name_is_refused() {
    let app = secured_router();
    let req = Request::builder()
        .uri("/v1/models")
        .header("host", "rebind.attacker.example:11434")
        .header("authorization", "Bearer sekrit")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::MISDIRECTED_REQUEST);
}

/// A gate that, like the node's, can change its token while running.
#[derive(Debug, Default)]
struct TestGate {
    token: std::sync::Mutex<Option<String>>,
    let_in: std::sync::atomic::AtomicUsize,
}

impl ApiGate for TestGate {
    fn accepts(&self, presented: &str) -> bool {
        self.token.lock().unwrap().as_deref() == Some(presented)
    }
    fn let_in(&self) {
        self.let_in
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn gated_router(gate: Arc<TestGate>) -> axum::Router {
    router(
        Arc::new(EchoBackend),
        ApiSecurity {
            // Ignored once a gate decides.
            auth_token: Some("static".into()),
            gate: Some(gate),
            ..Default::default()
        },
    )
}

async fn status_of(app: axum::Router, path: &str, token: Option<&str>) -> StatusCode {
    let mut req = Request::builder().uri(path);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    app.oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_gate_lets_in_its_token_and_nothing_else() {
    let gate = Arc::new(TestGate::default());
    *gate.token.lock().unwrap() = Some("the-token".into());
    let app = gated_router(gate.clone());
    assert_eq!(
        status_of(app.clone(), "/healthz", None).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(app.clone(), "/v1/models", Some("the-token")).await,
        StatusCode::OK
    );
    assert_eq!(
        status_of(app.clone(), "/v1/models", Some("static")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_of(app.clone(), "/v1/models", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(gate.let_in.load(std::sync::atomic::Ordering::Relaxed), 1);

    // A new token replaces the old one at once.
    *gate.token.lock().unwrap() = Some("rotated".into());
    assert_eq!(
        status_of(app.clone(), "/v1/models", Some("the-token")).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_of(app, "/v1/models", Some("rotated")).await,
        StatusCode::OK
    );
}

async fn post_json(app: axum::Router, uri: &str, body: serde_json::Value) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

#[tokio::test]
async fn chat_completions_accept_content_as_parts() {
    let (status, body) = post_json(
        test_router(),
        "/v1/chat/completions",
        serde_json::json!({
            "model": "nepheral-standard",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": "there"}
            ]}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "echo: hello\nthere");
}

#[tokio::test]
async fn chat_completions_refuse_images_with_an_openai_error() {
    let (status, body) = post_json(
        test_router(),
        "/v1/chat/completions",
        serde_json::json!({
            "model": "nepheral-standard",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
            ]}]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "unsupported_content");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("image_url"));
}

#[tokio::test]
async fn malformed_json_gets_an_openai_error() {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            "{\"model\": \"nepheral-standard\", \"messages\": [{]",
        ))
        .unwrap();
    let res = test_router().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], "invalid_json");
}

#[tokio::test]
async fn stop_sequences_cut_the_answer() {
    let (status, body) = post_json(
        test_router(),
        "/v1/chat/completions",
        serde_json::json!({
            "model": "nepheral-standard",
            "messages": [{"role": "user", "content": "one two three"}],
            "stop": " two"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "echo: one");

    let (_, body) = post_json(
        test_router(),
        "/api/chat",
        serde_json::json!({
            "model": "nepheral-standard",
            "stream": false,
            "messages": [{"role": "user", "content": "one two three"}],
            "options": {"stop": ["three"], "temperature": 0.2, "num_predict": 64}
        }),
    )
    .await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["message"]["content"], "echo: one two ");
}

#[tokio::test]
async fn a_stop_sequence_split_across_pieces_is_caught() {
    let pieces = async_stream::stream! {
        for p in ["Hello wor", "ld, END", "ING more"] {
            yield BackendEvent::Token(p.to_string());
        }
        yield BackendEvent::Done;
    };
    let mut out = nepheral_api::backend::stop_at(Box::pin(pieces), vec!["ENDING".into()]);
    let mut text = String::new();
    while let Some(ev) = out.next().await {
        if let BackendEvent::Token(t) = ev {
            text.push_str(&t);
        }
    }
    assert_eq!(text, "Hello world, ");
}

#[tokio::test]
async fn generation_options_reach_the_backend() {
    use std::sync::Mutex;
    struct Recorder(Mutex<Option<nepheral_api::backend::GenerationOptions>>);
    #[async_trait]
    impl MeshBackend for Recorder {
        async fn list_models(&self) -> Vec<ModelInfo> {
            Vec::new()
        }
        async fn submit(&self, request: ChatRequest) -> Result<EventStream, BackendError> {
            *self.0.lock().unwrap() = Some(request.options.clone());
            Ok(Box::pin(futures::stream::iter([BackendEvent::Done])))
        }
    }
    let backend = Arc::new(Recorder(Mutex::new(None)));
    let app = router(
        backend.clone(),
        ApiSecurity {
            allow_unauthenticated: true,
            ..Default::default()
        },
    );
    let (status, _) = post_json(
        app,
        "/v1/chat/completions",
        serde_json::json!({
            "model": "nepheral-local",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.1,
            "max_completion_tokens": 32,
            "stop": ["\n\n", "END"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let o = backend.0.lock().unwrap().clone().expect("submitted");
    assert_eq!(o.temperature, Some(0.1));
    assert_eq!(o.max_tokens, Some(32));
    assert_eq!(o.stop, vec!["\n\n".to_string(), "END".to_string()]);
}
