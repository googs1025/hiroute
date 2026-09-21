#![cfg(feature = "pingora-transport")]

use std::error::Error;
use std::io;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hiroute_gateway_core::core::execution_plan::{PlanRevision, ResolvedTargetBindingId};
use hiroute_gateway_core::core::publication::{PrepareOutcome, PublicationInstaller};
use hiroute_gateway_core::runtime::body::BodyPlan;
use hiroute_gateway_core::runtime::driver::{
    GatewayCoreLifecycle, GatewayCoreLifecycleLimits, NoopGatewayFilterManager,
};
use hiroute_gateway_core::test_support::lifecycle::{
    BootstrapSelection, PassthroughBootstrapProvider,
};
use hiroute_gateway_core::test_support::{
    BootstrapBodyPlans, BootstrapPublicationBuilder, plain_target,
};
use hiroute_gateway_core::transport::pingora::{GatewayHttpApp, PingoraConnectorAdapter};
use hiroute_gateway_core::transport::{
    GatewayLifecycle, GatewaySession, SessionReuse, TransportError,
};
use pingora_core::server::ShutdownWatch;
use pingora_core::services::Service as ServiceContract;
use pingora_core::services::listening::Service as ListeningService;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

type HarnessError = Box<dyn Error + Send + Sync>;

fn duration_from_env(name: &str, default_seconds: u64) -> Duration {
    Duration::from_secs(
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default_seconds),
    )
}

fn connection_counts() -> Vec<usize> {
    std::env::var("HIROUTE_STRESS_CONNECTIONS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .filter_map(|part| part.trim().parse().ok())
                .collect()
        })
        .filter(|values: &Vec<usize>| !values.is_empty())
        .unwrap_or_else(|| vec![1_000, 10_000])
}

fn sse_plan() -> BodyPlan {
    BodyPlan::SseFramedStreaming {
        max_event_bytes: 64 * 1024,
        max_pending_bytes: 64 * 1024,
        max_output_event_bytes: 128 * 1024,
        expansion_ratio_numerator: 2,
        expansion_ratio_denominator: 1,
        expansion_slack_bytes: 64,
    }
}

fn lifecycle_plans(max_prompt_bytes: usize) -> BootstrapBodyPlans {
    BootstrapBodyPlans {
        logical_request: BodyPlan::BufferedTransform {
            max_body_bytes: max_prompt_bytes,
        },
        attempt_request: BodyPlan::StreamingReplay {
            max_chunk_bytes: 16 * 1024,
            max_replay_bytes: max_prompt_bytes,
        },
        attempt_response_precommit: sse_plan(),
        accepted_response: sse_plan(),
    }
}

fn install_publication(
    upstream: SocketAddr,
    plan_revision: u64,
    max_prompt_bytes: usize,
    queue_capacity: usize,
) -> Result<(Arc<PublicationInstaller>, ResolvedTargetBindingId), HarnessError> {
    let plan = PlanRevision(plan_revision);
    let binding = ResolvedTargetBindingId::new(plan, 1);
    let envelope = BootstrapPublicationBuilder::new(plan.0, plan.0)
        .route("gateway.test", "/", 1, plain_target(upstream, plan.0))?
        .body_plans(1, lifecycle_plans(max_prompt_bytes))?
        .body_queue_capacities(1, queue_capacity, queue_capacity)?
        .build()?;
    let installer = Arc::new(PublicationInstaller::new());
    let cancellation = CancellationToken::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let prepared = match installer.prepare(envelope, &cancellation, deadline)? {
        PrepareOutcome::Prepared(prepared) => prepared,
        PrepareOutcome::Duplicate(_) => return Err("unexpected duplicate publication".into()),
    };
    installer.publish(prepared, &cancellation, deadline)?;
    Ok((installer, binding))
}

type StabilityLifecycle = GatewayCoreLifecycle<
    BootstrapSelection,
    PassthroughBootstrapProvider,
    NoopGatewayFilterManager,
    PingoraConnectorAdapter,
>;

#[derive(Clone)]
struct ObservedLifecycle {
    inner: Arc<StabilityLifecycle>,
    errors: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl GatewayLifecycle for ObservedLifecycle {
    async fn process(
        &self,
        session: &mut dyn GatewaySession,
    ) -> Result<SessionReuse, TransportError> {
        let result = self.inner.process(session).await;
        if let Err(error) = &result {
            self.errors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(error.to_string());
        }
        result
    }
}

fn production_lifecycle(
    upstream: SocketAddr,
    plan_revision: u64,
    max_prompt_bytes: usize,
    queue_capacity: usize,
    bootstrap_hard_cap: Duration,
) -> Result<
    (
        Arc<StabilityLifecycle>,
        BootstrapSelection,
        PassthroughBootstrapProvider,
    ),
    HarnessError,
> {
    let (publications, binding) =
        install_publication(upstream, plan_revision, max_prompt_bytes, queue_capacity)?;
    let selection = BootstrapSelection::new(binding);
    let provider = PassthroughBootstrapProvider::accepting_first_semantic_sse();
    let lifecycle = GatewayCoreLifecycle::new(
        publications,
        selection.clone(),
        provider.clone(),
        NoopGatewayFilterManager,
        PingoraConnectorAdapter::new(),
        GatewayCoreLifecycleLimits {
            max_request_body_bytes: max_prompt_bytes,
            write_quantum: 16 * 1024,
            bootstrap_hard_cap: Some(bootstrap_hard_cap),
            cleanup_timeout: Duration::from_secs(2),
            process_memory_bytes: 1024 * 1024 * 1024,
            worker_memory_bytes: 1024 * 1024 * 1024,
            stream_memory_bytes: 16 * 1024 * 1024,
            ..GatewayCoreLifecycleLimits::default()
        },
    )?;
    Ok((Arc::new(lifecycle), selection, provider))
}

fn reserve_tcp_port() -> io::Result<u16> {
    Ok(StdTcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn start_gateway_service(
    port: u16,
    lifecycle: Arc<StabilityLifecycle>,
    errors: Arc<Mutex<Vec<String>>>,
    shutdown: ShutdownWatch,
) -> JoinHandle<()> {
    let mut service = ListeningService::new(
        format!("gateway-core-stability-{port}"),
        GatewayHttpApp::new(ObservedLifecycle {
            inner: lifecycle,
            errors,
        }),
    );
    service.add_tcp(&format!("127.0.0.1:{port}"));
    tokio::spawn(async move {
        #[cfg(unix)]
        ServiceContract::start_service(&mut service, None, shutdown, 1).await;
        #[cfg(not(unix))]
        ServiceContract::start_service(&mut service, shutdown, 1).await;
    })
}

async fn wait_until_listening(address: SocketAddr) -> Result<(), HarnessError> {
    for _ in 0..200 {
        if TcpStream::connect(address).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(format!("gateway listener {address} did not start").into())
}

async fn stop_gateway_service(
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
) -> Result<(), HarnessError> {
    let _ = shutdown.send(true);
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .map_err(|_| "gateway stability service did not stop")??;
    Ok(())
}

async fn read_h1_head(stream: &mut TcpStream) -> Result<Vec<u8>, HarnessError> {
    let mut head = Vec::with_capacity(512);
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await?;
        head.push(byte[0]);
        if head.len() > 16 * 1024 {
            return Err("H1 request head exceeded harness limit".into());
        }
    }
    Ok(head)
}

fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(0)
}

async fn write_chunk(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    stream
        .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
        .await?;
    stream.write_all(bytes).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await
}

async fn serve_sse_connection(
    mut stream: TcpStream,
    event_duration: Duration,
    early_response: bool,
    event_interval: Duration,
) -> Result<usize, HarnessError> {
    let head = read_h1_head(&mut stream).await?;
    let request_bytes = content_length(&head);
    let response_head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    if early_response {
        stream.write_all(response_head).await?;
        write_chunk(&mut stream, b": precommit-heartbeat\n\n").await?;
        write_chunk(&mut stream, b"data: accepted\n\n").await?;
    }
    let mut body = vec![0_u8; request_bytes];
    stream.read_exact(&mut body).await?;
    if !early_response {
        stream.write_all(response_head).await?;
        write_chunk(&mut stream, b": precommit-heartbeat\n\n").await?;
        write_chunk(&mut stream, b"data: accepted\n\n").await?;
    }
    let deadline = Instant::now() + event_duration;
    let mut events = 0;
    while Instant::now() < deadline || events < 2 {
        write_chunk(&mut stream, b"data: stable-token\n\n").await?;
        events += 1;
        if !event_duration.is_zero() {
            tokio::time::sleep(event_interval.min(event_duration)).await;
        }
    }
    stream.write_all(b"0\r\n\r\n").await?;
    stream.flush().await?;
    Ok(events + 2)
}

async fn run_prompt_case(
    prompt_bytes: usize,
    early_response: bool,
    event_duration: Duration,
    plan_revision: u64,
) -> Result<(), HarnessError> {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_address = upstream_listener.local_addr()?;
    let upstream = tokio::spawn(async move {
        let (stream, _) = upstream_listener.accept().await?;
        serve_sse_connection(
            stream,
            event_duration,
            early_response,
            Duration::from_millis(20),
        )
        .await
    });
    // Pingora is free to surface request DATA below the attempt write quantum;
    // capacity is a frame bound independent from the 4 MiB byte bound.
    let queue_capacity = (prompt_bytes / 4_096).saturating_add(16);
    let (lifecycle, selection, provider) = production_lifecycle(
        upstream_address,
        plan_revision,
        4 * 1024 * 1024,
        queue_capacity,
        event_duration + Duration::from_secs(30),
    )?;
    let port = reserve_tcp_port()?;
    let gateway_address = SocketAddr::from(([127, 0, 0, 1], port));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let lifecycle_errors = Arc::new(Mutex::new(Vec::new()));
    let gateway = start_gateway_service(
        port,
        Arc::clone(&lifecycle),
        Arc::clone(&lifecycle_errors),
        shutdown_rx,
    );
    wait_until_listening(gateway_address).await?;

    let mut client = TcpStream::connect(gateway_address).await?;
    client
        .write_all(
            format!(
                "POST /prompt HTTP/1.1\r\nHost: gateway.test\r\nContent-Length: {prompt_bytes}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let prompt = vec![b'p'; prompt_bytes];
    let mut written = 0;
    for chunk in prompt.chunks(16 * 1024) {
        client.write_all(chunk).await.map_err(|error| {
            let errors = lifecycle_errors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            format!(
                "downstream prompt write failed at {written}/{prompt_bytes}: {error}; provider bytes={}; lifecycle errors: {errors:?}",
                provider.logical_body_bytes()
            )
        })?;
        written += chunk.len();
        tokio::task::yield_now().await;
    }
    client.flush().await?;
    drop(prompt);
    let mut response = Vec::new();
    let mut scratch = [0_u8; 257];
    loop {
        let read = client.read(&mut scratch).await?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&scratch[..read]);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let upstream_result = upstream.await?;
    if let Err(error) = upstream_result {
        let errors = lifecycle_errors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        return Err(format!("upstream failed: {error}; lifecycle errors: {errors:?}").into());
    }
    let upstream_events = upstream_result.expect("checked successful upstream result");
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(
        response
            .windows(14)
            .any(|window| window == b"data: accepted")
    );
    assert!(upstream_events >= 4);
    assert_eq!(selection.selected(), 1);
    assert_eq!(selection.published(), 1);
    assert_eq!(provider.terminal_releases(), 1);
    assert!(provider.classified_sse_events() >= 2);
    assert!(provider.encoded_sse_events() >= upstream_events);
    assert!(
        lifecycle_errors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "production lifecycle returned an error"
    );

    stop_gateway_service(shutdown_tx, gateway).await?;
    let snapshot = lifecycle.budget_snapshot();
    assert_eq!(snapshot.process_live, 0);
    assert_eq!(snapshot.active_streams, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the dedicated 60-second real-lifecycle runner"]
async fn dedicated_prompt_and_sixty_second_sse_memory_stability() -> Result<(), HarnessError> {
    let duration = duration_from_env("HIROUTE_PROMPT_SSE_SECONDS", 60);
    let mut revision = 20_000;
    for prompt_bytes in [1024, 4 * 1024 * 1024] {
        for early_response in [true, false] {
            run_prompt_case(prompt_bytes, early_response, duration, revision)
                .await
                .map_err(|error| {
                    format!(
                        "prompt lifecycle failed: bytes={prompt_bytes}, early={early_response}: {error}"
                    )
                })?;
            revision += 1;
        }
    }
    Ok(())
}

async fn connect_slow_sse_client(
    gateway: SocketAddr,
    deadline: Instant,
) -> Result<usize, HarnessError> {
    let mut stream = loop {
        match TcpStream::connect(gateway).await {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: gateway.test\r\nConnection: close\r\n\r\n")
        .await?;
    let mut total = 0;
    let mut scratch = [0_u8; 64];
    loop {
        let read = stream.read(&mut scratch).await?;
        if read == 0 {
            break;
        }
        total += read;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(total)
}

async fn run_socket_count(
    count: usize,
    duration: Duration,
    revision: u64,
) -> Result<(), HarnessError> {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
    let upstream_address = upstream_listener.local_addr()?;
    let upstream = tokio::spawn(async move {
        let mut handlers = JoinSet::new();
        for _ in 0..count {
            let (stream, _) = upstream_listener.accept().await?;
            handlers.spawn(serve_sse_connection(
                stream,
                duration,
                true,
                Duration::from_millis(100),
            ));
        }
        while let Some(result) = handlers.join_next().await {
            result??;
        }
        Ok::<(), HarnessError>(())
    });
    let (lifecycle, selection, provider) = production_lifecycle(
        upstream_address,
        revision,
        1024,
        1,
        duration + Duration::from_secs(60),
    )?;
    let port = reserve_tcp_port()?;
    let gateway_address = SocketAddr::from(([127, 0, 0, 1], port));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let lifecycle_errors = Arc::new(Mutex::new(Vec::new()));
    let gateway = start_gateway_service(
        port,
        Arc::clone(&lifecycle),
        Arc::clone(&lifecycle_errors),
        shutdown_rx,
    );
    wait_until_listening(gateway_address).await?;

    let connect_deadline = Instant::now() + Duration::from_secs(120);
    let mut clients = JoinSet::new();
    for _ in 0..count {
        clients.spawn(connect_slow_sse_client(gateway_address, connect_deadline));
    }
    let mut completed = 0;
    while let Some(result) = clients.join_next().await {
        assert!(result?? > 0);
        completed += 1;
    }
    assert_eq!(completed, count);
    upstream.await??;
    assert_eq!(selection.selected(), count);
    assert_eq!(selection.published(), count);
    assert_eq!(provider.terminal_releases(), count);
    assert!(
        lifecycle_errors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "production lifecycle returned errors"
    );

    stop_gateway_service(shutdown_tx, gateway).await?;
    let snapshot = lifecycle.budget_snapshot();
    assert_eq!(snapshot.process_live, 0);
    assert_eq!(snapshot.active_streams, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "requires the dedicated ten-minute real-socket slow-sink runner"]
async fn dedicated_1k_10k_sse_connection_churn_for_ten_minutes() -> Result<(), HarnessError> {
    let duration = duration_from_env("HIROUTE_CONNECTION_STRESS_SECONDS", 600);
    for (index, count) in connection_counts().into_iter().enumerate() {
        run_socket_count(count, duration, 30_000 + index as u64).await?;
    }
    Ok(())
}
use async_trait::async_trait;
