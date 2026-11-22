//! What the client sees when upstream answers.
//!
//! The proxy used to answer `200` the moment it had a channel to write into,
//! which meant a refusal arrived as a successful response carrying an error
//! document. The client then failed to unmarshal it and reported a schema error
//! — `body.data.id Field required` — with the status and the reason both lost.
//! That is how a `reasoning_effort` the model does not support reached the user
//! as a validation error.
//!
//! This test stands a fake upstream in front of the proxy and asserts both
//! halves of the contract: a refusal keeps its own status and its own reason,
//! and an answer is relayed unchanged. It is one test in its own binary because
//! it sets process-wide environment variables, and a second test in the same
//! process would race it.

use std::path::PathBuf;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use vibedesk_lib::capabilities::proxy;
use vibedesk_lib::capabilities::{registry, CapabilityId};

/// The refusal upstream actually sends for a level the model does not offer.
const REFUSAL: &str = r#"{"object":"error","message":"reasoning_effort 'medium' is not supported for this model; supported values: ['none', 'low', 'high', 'xhigh', 'max']","type":"invalid_request_invalid_args","param":null,"code":"3051","raw_status_code":400}"#;

/// A completion in the shape the client parses.
const ANSWER: &str = r#"{"id":"cmpl-1","object":"chat.completion","created":1,"model":"fake-model","choices":[{"index":0,"message":{"role":"assistant","content":"OK"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vibedesk-proxy-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("a scratch directory");
    path
}

/// A server that refuses a request naming `refuse` and answers anything else.
async fn fake_upstream() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("a port");
    let port = listener.local_addr().expect("an address").port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let service = service_fn(|request: Request<Incoming>| async {
                    let body = request.into_body().collect().await.map(|c| c.to_bytes());
                    let asked = body
                        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
                        .unwrap_or_default();
                    let (status, answer) = if asked.contains("refuse") {
                        (StatusCode::BAD_REQUEST, REFUSAL)
                    } else {
                        (StatusCode::OK, ANSWER)
                    };
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .status(status)
                            .header("content-type", "application/json")
                            .body(Full::new(Bytes::from(answer)))
                            .expect("a response"),
                    )
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    port
}

#[tokio::test]
async fn a_refusal_keeps_its_status_and_an_answer_is_relayed() {
    let upstream = fake_upstream().await;

    // The proxy reads its upstream from `config.toml` and its own settings from
    // the app data directory, so both are pointed at scratch copies.
    let home = scratch("home");
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[[providers]]\nname = \"fake\"\napi_base = \"http://127.0.0.1:{upstream}/v1\"\napi_key_env_var = \"FAKE_KEY\"\nbackend = \"openai\"\n"
        ),
    )
    .expect("a config");

    let data = scratch("data");
    std::fs::write(
        data.join("capabilities.json"),
        r#"{"enabled":{"compaction":true},"compaction":{"upstreamProvider":"fake","proxyPort":0}}"#,
    )
    .expect("capability settings");

    std::env::set_var("VIBE_HOME", &home);
    std::env::set_var("VIBEDESK_DATA_DIR", &data);
    registry().set_enabled(CapabilityId::Compaction, true);

    let port = proxy::start(0).await.expect("the proxy starts");
    let client = reqwest::Client::new();
    let ask = |model: &'static str| {
        client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .header("authorization", "Bearer test")
            .json(&serde_json::json!({
                "model": model,
                "stream": false,
                "messages": [{ "role": "user", "content": "say OK" }],
            }))
            .send()
    };

    let refused = ask("refuse").await.expect("the proxy answers");
    let status = refused.status();
    let body = refused.text().await.expect("a body");
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a refusal must keep upstream's status, not arrive as a 200: {body}"
    );
    assert!(
        body.contains("reasoning_effort 'medium' is not supported"),
        "the reason must reach the client verbatim: {body}"
    );

    let answered = ask("fake-model").await.expect("the proxy answers");
    let status = answered.status();
    let body = answered.text().await.expect("a body");
    assert_eq!(status, StatusCode::OK, "an answer is a 200: {body}");
    assert!(
        body.contains("\"content\":\"OK\""),
        "the answer must be relayed unchanged: {body}"
    );
}
