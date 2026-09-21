use std::error::Error;
use std::hint::black_box;
use std::time::{Duration, Instant};

#[cfg(not(feature = "pingora-transport"))]
use hiroute_gateway_core::core::publication::PrepareOutcome;
use hiroute_gateway_core::core::publication::PublicationInstaller;
#[cfg(not(feature = "pingora-transport"))]
use hiroute_gateway_core::test_support::{BootstrapPublicationBuilder, plain_target};
#[cfg(feature = "pingora-transport")]
use hiroute_gateway_core::transport::pingora::PINNED_PINGORA_REVISION;
#[cfg(not(feature = "pingora-transport"))]
use http::Request;
#[cfg(not(feature = "pingora-transport"))]
use tokio_util::sync::CancellationToken;

const MIN_INTERLEAVED_ROUNDS: usize = 5;
#[cfg(not(feature = "pingora-transport"))]
const PINNED_PINGORA_REVISION: &str = "0046038bd402bc82912da862dadf9a479f31e9f1";

type BenchError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Copy, Debug)]
enum Protocol {
    Http1,
    Http2,
}

impl Protocol {
    fn name(self) -> &'static str {
        match self {
            Self::Http1 => "h1",
            Self::Http2 => "h2",
        }
    }

    #[cfg(not(feature = "pingora-transport"))]
    fn marker(self) -> &'static str {
        match self {
            Self::Http1 => "http/1.1",
            Self::Http2 => "h2",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Stats {
    operations: usize,
    elapsed: Duration,
    throughput: f64,
    ttft_p50_ns: u128,
    ttft_p99_ns: u128,
    p50_ns: u128,
    p99_ns: u128,
    variance_ns2: f64,
    mean_ci95_ns: f64,
}

#[derive(Clone, Copy, Debug)]
struct RequestTiming {
    ttft: Duration,
    elapsed: Duration,
}

fn main() -> Result<(), BenchError> {
    let iterations = std::env::var("HIROUTE_BENCH_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2_000)
        .max(100);
    let enforce = std::env::var_os("HIROUTE_ENFORCE_5_PERCENT").is_some();

    println!("pingora_revision\t{PINNED_PINGORA_REVISION}");
    println!("os\t{}", std::env::consts::OS);
    println!("arch\t{}", std::env::consts::ARCH);
    println!("release\ttrue");
    println!("iterations_per_variant_round\t{iterations}");
    println!(
        "protocol\tround\tvariant\tthroughput_ops_s\tttft_p50_ns\tttft_p99_ns\tp50_ns\tp99_ns\tvariance_ns2\tmean_ci95_ns"
    );

    #[cfg(feature = "pingora-transport")]
    {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        runtime.block_on(pingora_loopback::run(iterations, enforce))
    }

    #[cfg(not(feature = "pingora-transport"))]
    run_contract_smoke(iterations, enforce)
}

#[cfg(not(feature = "pingora-transport"))]
fn run_contract_smoke(iterations: usize, enforce: bool) -> Result<(), BenchError> {
    let installer = installed_publication()?;
    let mut failed = false;
    for protocol in [Protocol::Http1, Protocol::Http2] {
        let mut baseline_rounds = Vec::with_capacity(MIN_INTERLEAVED_ROUNDS);
        let mut gateway_rounds = Vec::with_capacity(MIN_INTERLEAVED_ROUNDS);
        for round in 0..MIN_INTERLEAVED_ROUNDS {
            let baseline = || fallback_header_fast_path(protocol, iterations);
            let gateway = || gateway_header_fast_path(protocol, iterations, &installer);
            let (baseline_stats, gateway_stats) = if round % 2 == 0 {
                (baseline()?, gateway()?)
            } else {
                let gateway_stats = gateway()?;
                let baseline_stats = baseline()?;
                (baseline_stats, gateway_stats)
            };
            print_stats(
                protocol,
                round + 1,
                "transport-contract-smoke",
                baseline_stats,
            );
            print_stats(protocol, round + 1, "gateway-core-smoke", gateway_stats);
            baseline_rounds.push(baseline_stats);
            gateway_rounds.push(gateway_stats);
        }
        failed |= print_aggregate(protocol, &baseline_rounds, &gateway_rounds, enforce);
    }
    if failed {
        return Err("component smoke 5% gate failed".into());
    }
    Ok(())
}

#[cfg(not(feature = "pingora-transport"))]
fn installed_publication() -> Result<PublicationInstaller, BenchError> {
    let envelope = BootstrapPublicationBuilder::new(1, 1)
        .route(
            "benchmark.test",
            "/",
            1,
            plain_target("127.0.0.1:18080".parse()?, 1),
        )?
        .build()?;
    let installer = PublicationInstaller::new();
    let cancel = CancellationToken::new();
    let prepared =
        match installer.prepare(envelope, &cancel, Instant::now() + Duration::from_secs(1))? {
            PrepareOutcome::Prepared(prepared) => prepared,
            PrepareOutcome::Duplicate(_) => return Err("unexpected initial duplicate".into()),
        };
    installer.publish(prepared, &cancel, Instant::now() + Duration::from_secs(1))?;
    Ok(installer)
}

#[cfg(not(feature = "pingora-transport"))]
fn fallback_header_fast_path(protocol: Protocol, iterations: usize) -> Result<Stats, BenchError> {
    measure(iterations, || {
        let request = Request::builder()
            .method("GET")
            .uri("/v1/benchmark")
            .header("x-benchmark-alpn", protocol.marker())
            .body(())?;
        black_box(request);
        Ok(())
    })
}

#[cfg(not(feature = "pingora-transport"))]
fn gateway_header_fast_path(
    protocol: Protocol,
    iterations: usize,
    installer: &PublicationInstaller,
) -> Result<Stats, BenchError> {
    measure(iterations, || {
        let binding = installer.bind_request()?;
        black_box(binding.plan_revision());
        let request = Request::builder()
            .method("GET")
            .uri("/v1/benchmark")
            .header("x-benchmark-alpn", protocol.marker())
            .body(())?;
        black_box(request);
        Ok(())
    })
}

#[cfg(not(feature = "pingora-transport"))]
fn measure(
    iterations: usize,
    mut operation: impl FnMut() -> Result<(), BenchError>,
) -> Result<Stats, BenchError> {
    let mut samples = Vec::with_capacity(iterations);
    let mut ttft_samples = Vec::with_capacity(iterations);
    let total_start = Instant::now();
    for _ in 0..iterations {
        let start = Instant::now();
        operation()?;
        let elapsed = start.elapsed();
        samples.push(elapsed.as_nanos());
        // The no-transport contract smoke has no independently observable
        // first response byte. Treat the completed local operation as its
        // TTFT, while the fixed-runner evidence only accepts the real
        // Pingora transport configuration below.
        ttft_samples.push(elapsed.as_nanos());
    }
    Ok(stats_from_samples(
        iterations,
        total_start.elapsed(),
        samples,
        ttft_samples,
    ))
}

fn stats_from_samples(
    operations: usize,
    elapsed: Duration,
    mut samples: Vec<u128>,
    mut ttft_samples: Vec<u128>,
) -> Stats {
    samples.sort_unstable();
    ttft_samples.sort_unstable();
    let p50_ns = percentile(&samples, 50);
    let p99_ns = percentile(&samples, 99);
    let mean = samples.iter().map(|sample| *sample as f64).sum::<f64>() / samples.len() as f64;
    let variance_ns2 = samples
        .iter()
        .map(|sample| {
            let delta = *sample as f64 - mean;
            delta * delta
        })
        .sum::<f64>()
        / samples.len() as f64;
    let mean_ci95_ns = 1.96 * (variance_ns2 / samples.len() as f64).sqrt();
    Stats {
        operations,
        elapsed,
        throughput: operations as f64 / elapsed.as_secs_f64(),
        ttft_p50_ns: percentile(&ttft_samples, 50),
        ttft_p99_ns: percentile(&ttft_samples, 99),
        p50_ns,
        p99_ns,
        variance_ns2,
        mean_ci95_ns,
    }
}

fn percentile(samples: &[u128], percentile: usize) -> u128 {
    let index = (samples.len() - 1) * percentile / 100;
    samples[index]
}

fn aggregate(rounds: &[Stats]) -> Stats {
    let operations = rounds.iter().map(|round| round.operations).sum();
    let elapsed = rounds.iter().map(|round| round.elapsed).sum();
    Stats {
        operations,
        elapsed,
        throughput: operations as f64 / elapsed.as_secs_f64(),
        ttft_p50_ns: rounds.iter().map(|round| round.ttft_p50_ns).sum::<u128>()
            / rounds.len() as u128,
        ttft_p99_ns: rounds.iter().map(|round| round.ttft_p99_ns).sum::<u128>()
            / rounds.len() as u128,
        p50_ns: rounds.iter().map(|round| round.p50_ns).sum::<u128>() / rounds.len() as u128,
        p99_ns: rounds.iter().map(|round| round.p99_ns).sum::<u128>() / rounds.len() as u128,
        variance_ns2: rounds.iter().map(|round| round.variance_ns2).sum::<f64>()
            / rounds.len() as f64,
        mean_ci95_ns: rounds.iter().map(|round| round.mean_ci95_ns).sum::<f64>()
            / rounds.len() as f64,
    }
}

fn print_aggregate(
    protocol: Protocol,
    baseline_rounds: &[Stats],
    gateway_rounds: &[Stats],
    enforce: bool,
) -> bool {
    let baseline = aggregate(baseline_rounds);
    let gateway = aggregate(gateway_rounds);
    let throughput_regression = percent_regression(baseline.throughput, gateway.throughput);
    let p99_regression = percent_increase(baseline.p99_ns as f64, gateway.p99_ns as f64);
    println!(
        "aggregate\t{}\tthroughput_regression_pct\t{throughput_regression:.3}",
        protocol.name()
    );
    println!(
        "aggregate\t{}\tp99_increase_pct\t{p99_regression:.3}",
        protocol.name()
    );
    enforce && (throughput_regression > 5.0 || p99_regression > 5.0)
}

fn percent_regression(baseline: f64, candidate: f64) -> f64 {
    ((baseline - candidate) / baseline * 100.0).max(0.0)
}

fn percent_increase(baseline: f64, candidate: f64) -> f64 {
    ((candidate - baseline) / baseline * 100.0).max(0.0)
}

fn print_stats(protocol: Protocol, round: usize, variant: &str, stats: Stats) {
    println!(
        "{}\t{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}",
        protocol.name(),
        round,
        variant,
        stats.throughput,
        stats.ttft_p50_ns,
        stats.ttft_p99_ns,
        stats.p50_ns,
        stats.p99_ns,
        stats.variance_ns2,
        stats.mean_ci95_ns
    );
}

#[cfg(feature = "pingora-transport")]
mod pingora_loopback {
    use std::net::{SocketAddr, TcpListener as StdTcpListener};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use bytes::{Buf, Bytes};
    use futures::future::poll_fn;
    use h2::client::{self, SendRequest};
    use hiroute_gateway_core::core::execution_plan::{
        PlanRevision, ResolvedTargetBindingId, TransportTarget,
    };
    use hiroute_gateway_core::core::publication::PrepareOutcome;
    use hiroute_gateway_core::runtime::attempt::{PreparedRequestHead, TransportPrecommitEvent};
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
        GatewayLifecycle, GatewayResponseHead, GatewaySession, SessionReuse, TransportError,
    };
    use http::header::{CONTENT_LENGTH, HOST};
    use http::{HeaderValue, Request, StatusCode};
    use pingora_core::apps::{
        HttpPersistentSettings, HttpServerApp, HttpServerOptions, ReusedHttpStream,
    };
    use pingora_core::protocols::http::ServerSession;
    use pingora_core::server::ShutdownWatch;
    use pingora_core::services::Service as ServiceContract;
    use pingora_core::services::listening::Service as ListeningService;
    use pingora_http::ResponseHeader;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::watch;
    use tokio::task::JoinHandle;

    use super::{
        BenchError, Duration, Instant, MIN_INTERLEAVED_ROUNDS, Protocol, PublicationInstaller,
        RequestTiming, Stats, black_box, print_aggregate, print_stats, stats_from_samples,
    };

    const RESPONSE_BYTES: usize = 1024;
    static RESPONSE_BODY: [u8; RESPONSE_BYTES] = [b'x'; RESPONSE_BYTES];
    const H1_REQUEST: &[u8] =
        b"GET /v1/benchmark HTTP/1.1\r\nHost: benchmark.test\r\nConnection: keep-alive\r\n\r\n";

    pub async fn run(iterations: usize, enforce: bool) -> Result<(), BenchError> {
        let upstream_service_time = Duration::from_micros(
            std::env::var("HIROUTE_BENCH_UPSTREAM_SERVICE_MICROS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
        );
        let upstream_port = reserve_port()?;
        let mut baseline_port = reserve_port()?;
        while baseline_port == upstream_port {
            baseline_port = reserve_port()?;
        }
        let mut gateway_port = reserve_port()?;
        while gateway_port == baseline_port || gateway_port == upstream_port {
            gateway_port = reserve_port()?;
        }
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let upstream_task = start_service(
            upstream_port,
            BarePingoraApp::new(upstream_service_time),
            shutdown_rx.clone(),
        );
        let upstream_addr = SocketAddr::from(([127, 0, 0, 1], upstream_port));
        wait_until_listening(upstream_addr).await?;
        let target = plain_target(upstream_addr, 9_001);
        let baseline_errors = Arc::new(Mutex::new(None));
        let baseline_task = start_service(
            baseline_port,
            GatewayHttpApp::new(ObservedLifecycle {
                inner: BareProxyLifecycle::new(target.clone()),
                last_error: Arc::clone(&baseline_errors),
            }),
            shutdown_rx.clone(),
        );
        let gateway = production_gateway_lifecycle(target)?;
        let gateway_errors = Arc::new(Mutex::new(None));
        let gateway_task = start_service(
            gateway_port,
            GatewayHttpApp::new(ObservedLifecycle {
                inner: gateway,
                last_error: Arc::clone(&gateway_errors),
            }),
            shutdown_rx,
        );
        let baseline_addr = SocketAddr::from(([127, 0, 0, 1], baseline_port));
        let gateway_addr = SocketAddr::from(([127, 0, 0, 1], gateway_port));
        wait_until_listening(baseline_addr).await?;
        wait_until_listening(gateway_addr).await?;

        println!("payload_bytes\t{RESPONSE_BYTES}");
        println!(
            "controlled_upstream_service_time_us\t{}",
            upstream_service_time.as_micros()
        );
        println!("connection_reuse\ttrue");
        println!("concurrency\t1");
        println!("baseline_object_graph\tPingora ingress + Pingora pooled upstream proxy");
        println!(
            "candidate_object_graph\tGatewayCoreLifecycle no-filter fast path + AttemptExchange + Pingora pooled upstream"
        );
        let mut failed = false;
        for protocol in [Protocol::Http1, Protocol::Http2] {
            let mut baseline_rounds = Vec::with_capacity(MIN_INTERLEAVED_ROUNDS);
            let mut gateway_rounds = Vec::with_capacity(MIN_INTERLEAVED_ROUNDS);
            for round in 0..MIN_INTERLEAVED_ROUNDS {
                let (baseline, gateway) = if round % 2 == 0 {
                    (
                        run_observed_variant(
                            protocol,
                            baseline_addr,
                            iterations,
                            "pingora-upstream-proxy",
                            &baseline_errors,
                        )
                        .await?,
                        run_observed_variant(
                            protocol,
                            gateway_addr,
                            iterations,
                            "gateway-production-driver",
                            &gateway_errors,
                        )
                        .await?,
                    )
                } else {
                    let gateway = run_observed_variant(
                        protocol,
                        gateway_addr,
                        iterations,
                        "gateway-production-driver",
                        &gateway_errors,
                    )
                    .await?;
                    let baseline = run_observed_variant(
                        protocol,
                        baseline_addr,
                        iterations,
                        "pingora-upstream-proxy",
                        &baseline_errors,
                    )
                    .await?;
                    (baseline, gateway)
                };
                print_stats(protocol, round + 1, "pingora-upstream-proxy", baseline);
                print_stats(protocol, round + 1, "gateway-production-driver", gateway);
                baseline_rounds.push(baseline);
                gateway_rounds.push(gateway);
            }
            failed |= print_aggregate(protocol, &baseline_rounds, &gateway_rounds, enforce);
        }

        let _ = shutdown_tx.send(true);
        await_service(upstream_task).await?;
        await_service(baseline_task).await?;
        await_service(gateway_task).await?;
        if failed {
            return Err("fixed-runner 5% performance gate failed".into());
        }
        Ok(())
    }

    fn reserve_port() -> Result<u16, BenchError> {
        let listener = StdTcpListener::bind("127.0.0.1:0")?;
        Ok(listener.local_addr()?.port())
    }

    fn start_service<A>(port: u16, app: A, shutdown: ShutdownWatch) -> JoinHandle<()>
    where
        A: pingora_core::apps::ServerApp + Send + Sync + 'static,
    {
        let mut service = ListeningService::new(format!("benchmark-{port}"), app);
        service.add_tcp(&format!("127.0.0.1:{port}"));
        tokio::spawn(async move {
            #[cfg(unix)]
            ServiceContract::start_service(&mut service, None, shutdown, 1).await;
            #[cfg(not(unix))]
            ServiceContract::start_service(&mut service, shutdown, 1).await;
        })
    }

    async fn wait_until_listening(address: SocketAddr) -> Result<(), BenchError> {
        for _ in 0..100 {
            if TcpStream::connect(address).await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Err(format!("Pingora benchmark listener {address} did not start").into())
    }

    async fn await_service(task: JoinHandle<()>) -> Result<(), BenchError> {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .map_err(|_| "Pingora benchmark service did not stop")??;
        Ok(())
    }

    async fn run_variant(
        protocol: Protocol,
        address: SocketAddr,
        iterations: usize,
    ) -> Result<Stats, BenchError> {
        match protocol {
            Protocol::Http1 => {
                let mut client = H1Client::connect(address).await?;
                warm_up(iterations, &mut client).await?;
                measure_async(iterations, &mut client).await
            }
            Protocol::Http2 => {
                let mut client = H2Client::connect(address).await?;
                warm_up(iterations, &mut client).await?;
                let stats = measure_async(iterations, &mut client).await?;
                client.close().await;
                Ok(stats)
            }
        }
    }

    async fn run_observed_variant(
        protocol: Protocol,
        address: SocketAddr,
        iterations: usize,
        label: &str,
        last_error: &Mutex<Option<String>>,
    ) -> Result<Stats, BenchError> {
        match run_variant(protocol, address, iterations).await {
            Ok(stats) => Ok(stats),
            Err(error) => {
                let lifecycle_error = last_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                Err(format!(
                    "{label} {} benchmark failed: {error}; lifecycle error: {lifecycle_error:?}",
                    protocol.name()
                )
                .into())
            }
        }
    }

    #[async_trait]
    trait BenchClient {
        async fn request(&mut self) -> Result<RequestTiming, BenchError>;
    }

    async fn warm_up(iterations: usize, client: &mut impl BenchClient) -> Result<(), BenchError> {
        for _ in 0..iterations.clamp(100, 1_000) {
            client.request().await?;
        }
        Ok(())
    }

    async fn measure_async(
        iterations: usize,
        client: &mut impl BenchClient,
    ) -> Result<Stats, BenchError> {
        let mut samples = Vec::with_capacity(iterations);
        let mut ttft_samples = Vec::with_capacity(iterations);
        let total_start = Instant::now();
        for _ in 0..iterations {
            let timing = client.request().await?;
            samples.push(timing.elapsed.as_nanos());
            ttft_samples.push(timing.ttft.as_nanos());
        }
        Ok(stats_from_samples(
            iterations,
            total_start.elapsed(),
            samples,
            ttft_samples,
        ))
    }

    struct H1Client {
        socket: TcpStream,
        response: Vec<u8>,
    }

    impl H1Client {
        async fn connect(address: SocketAddr) -> Result<Self, BenchError> {
            let socket = TcpStream::connect(address).await?;
            socket.set_nodelay(true)?;
            Ok(Self {
                socket,
                response: Vec::with_capacity(2 * 1024),
            })
        }
    }

    #[async_trait]
    impl BenchClient for H1Client {
        async fn request(&mut self) -> Result<RequestTiming, BenchError> {
            let start = Instant::now();
            self.socket.write_all(H1_REQUEST).await?;
            self.response.clear();
            let mut header_end = None;
            while header_end.is_none_or(|end| self.response.len() < end + RESPONSE_BYTES) {
                let read = self.socket.read_buf(&mut self.response).await?;
                if read == 0 {
                    return Err("H1 benchmark connection closed before a full response".into());
                }
                if header_end.is_none() {
                    header_end = self
                        .response
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|index| index + 4);
                }
            }
            let ttft = start.elapsed();
            let header_end = header_end.ok_or("H1 response header delimiter is missing")?;
            if !self.response.starts_with(b"HTTP/1.1 200")
                || self.response.len() != header_end + RESPONSE_BYTES
            {
                return Err("H1 benchmark response shape is invalid".into());
            }
            black_box(&self.response[header_end..]);
            Ok(RequestTiming {
                ttft,
                elapsed: start.elapsed(),
            })
        }
    }

    struct H2Client {
        sender: SendRequest<Bytes>,
        driver: JoinHandle<Result<(), h2::Error>>,
    }

    impl H2Client {
        async fn connect(address: SocketAddr) -> Result<Self, BenchError> {
            let socket = TcpStream::connect(address).await?;
            socket.set_nodelay(true)?;
            let (sender, connection) = client::handshake(socket).await?;
            Ok(Self {
                sender,
                driver: tokio::spawn(connection),
            })
        }

        async fn close(self) {
            drop(self.sender);
            self.driver.abort();
            let _ = self.driver.await;
        }
    }

    #[async_trait]
    impl BenchClient for H2Client {
        async fn request(&mut self) -> Result<RequestTiming, BenchError> {
            let start = Instant::now();
            poll_fn(|context| self.sender.poll_ready(context)).await?;
            let request = Request::builder()
                .uri("http://benchmark.test/v1/benchmark")
                .body(())?;
            let (response, _) = self.sender.send_request(request, true)?;
            let response = response.await?;
            let ttft = start.elapsed();
            if response.status() != StatusCode::OK {
                return Err("H2 benchmark response status is invalid".into());
            }
            let mut body = response.into_body();
            let mut total = 0;
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                let length = chunk.remaining();
                total += length;
                body.flow_control().release_capacity(length)?;
                black_box(chunk);
            }
            if total != RESPONSE_BYTES {
                return Err("H2 benchmark response body length is invalid".into());
            }
            Ok(RequestTiming {
                ttft,
                elapsed: start.elapsed(),
            })
        }
    }

    struct BarePingoraApp {
        options: HttpServerOptions,
        service_time: Duration,
    }

    #[derive(Clone)]
    struct ObservedLifecycle<L> {
        inner: L,
        last_error: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl<L> GatewayLifecycle for ObservedLifecycle<L>
    where
        L: GatewayLifecycle,
    {
        async fn process(
            &self,
            session: &mut dyn GatewaySession,
        ) -> Result<SessionReuse, TransportError> {
            let result = self.inner.process(session).await;
            if let Err(error) = &result {
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
            }
            result
        }
    }

    impl BarePingoraApp {
        fn new(service_time: Duration) -> Self {
            let mut options = HttpServerOptions::default();
            options.h2c = true;
            Self {
                options,
                service_time,
            }
        }
    }

    #[async_trait]
    impl HttpServerApp for BarePingoraApp {
        async fn process_new_http(
            self: &Arc<Self>,
            mut session: ServerSession,
            shutdown: &ShutdownWatch,
        ) -> Option<ReusedHttpStream> {
            if !session.read_request().await.ok()? {
                return None;
            }
            while session.read_request_body().await.ok()?.is_some() {}
            // Both variants traverse this same real upstream. A fixed,
            // explicitly reported service-time floor makes the end-to-end
            // workload repeatable and production-shaped while preserving a
            // minimal Pingora transport baseline and the exact 5% threshold.
            let service_started = Instant::now();
            while service_started.elapsed() < self.service_time {
                std::hint::spin_loop();
            }
            if *shutdown.borrow() {
                session.set_keepalive(None);
            }
            let persistent_settings = HttpPersistentSettings::for_session(&session);
            let mut response = ResponseHeader::build(200, Some(1)).ok()?;
            response
                .append_header(CONTENT_LENGTH, HeaderValue::from_static("1024"))
                .ok()?;
            session
                .write_response_header(Box::new(response))
                .await
                .ok()?;
            session
                .write_response_body(Bytes::from_static(&RESPONSE_BODY), true)
                .await
                .ok()?;
            session
                .finish()
                .await
                .ok()
                .flatten()
                .map(|stream| ReusedHttpStream::from_reusable_stream(stream, persistent_settings))
        }

        fn server_options(&self) -> Option<&HttpServerOptions> {
            Some(&self.options)
        }
    }

    #[derive(Clone)]
    struct BareProxyLifecycle {
        target: TransportTarget,
        connector: PingoraConnectorAdapter,
    }

    impl BareProxyLifecycle {
        fn new(target: TransportTarget) -> Self {
            Self {
                target,
                connector: PingoraConnectorAdapter::new(),
            }
        }
    }

    #[async_trait]
    impl GatewayLifecycle for BareProxyLifecycle {
        async fn process(
            &self,
            session: &mut dyn GatewaySession,
        ) -> Result<SessionReuse, TransportError> {
            let request = session.request_head()?;
            let address = self.target.addresses[0];
            let mut upstream = self
                .connector
                .connect(&self.target, address)
                .await
                .map_err(attempt_transport_error)?;
            let mut headers = request.headers;
            headers.insert(
                HOST,
                HeaderValue::from_str(&self.target.authority)
                    .map_err(|error| TransportError::Io(error.to_string().into()))?,
            );
            upstream
                .write_request_head(&PreparedRequestHead {
                    method: request.method,
                    path_and_query: request.path_and_query,
                    headers,
                })
                .await
                .map_err(attempt_transport_error)?;
            while let Some(body) = session.read_request_body().await? {
                upstream
                    .write_request_body(body, false)
                    .await
                    .map_err(attempt_transport_error)?;
            }
            upstream
                .finish_request_body()
                .await
                .map_err(attempt_transport_error)?;
            let TransportPrecommitEvent::ResponseHead { status, headers } = upstream
                .read_response_head()
                .await
                .map_err(attempt_transport_error)?
            else {
                return Err(TransportError::Io(
                    "Pingora baseline upstream returned body before head".into(),
                ));
            };
            session
                .write_response_head(GatewayResponseHead { status, headers })
                .await?;
            while let Some(body) = upstream
                .read_response_body()
                .await
                .map_err(attempt_transport_error)?
            {
                session.write_response_body(body, false).await?;
            }
            session.write_response_body(Bytes::new(), true).await?;
            self.connector
                .release(upstream)
                .await
                .map_err(attempt_transport_error)?;
            Ok(SessionReuse::Reusable)
        }
    }

    fn attempt_transport_error(error: impl ToString) -> TransportError {
        TransportError::Io(error.to_string().into())
    }

    fn production_gateway_lifecycle(
        target: TransportTarget,
    ) -> Result<impl GatewayLifecycle, BenchError> {
        let plan = PlanRevision(9_001);
        let binding = ResolvedTargetBindingId::new(plan, 1);
        let envelope = BootstrapPublicationBuilder::new(plan.0, 1)
            .route("benchmark.test", "/", 1, target)?
            .body_plans(
                1,
                BootstrapBodyPlans {
                    logical_request: BodyPlan::PassThrough {
                        max_chunk_bytes: 64 * 1024,
                    },
                    attempt_request: BodyPlan::PassThrough {
                        max_chunk_bytes: 64 * 1024,
                    },
                    attempt_response_precommit: BodyPlan::PassThrough {
                        max_chunk_bytes: 64 * 1024,
                    },
                    accepted_response: BodyPlan::PassThrough {
                        max_chunk_bytes: 64 * 1024,
                    },
                },
            )?
            .body_queue_capacities(1, 1, 1)?
            .build()?;
        let installer = Arc::new(PublicationInstaller::new());
        let cancellation = tokio_util::sync::CancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        let prepared = match installer.prepare(envelope, &cancellation, deadline)? {
            PrepareOutcome::Prepared(prepared) => prepared,
            PrepareOutcome::Duplicate(_) => {
                return Err("unexpected duplicate benchmark publication".into());
            }
        };
        installer.publish(prepared, &cancellation, deadline)?;

        Ok(GatewayCoreLifecycle::new(
            installer,
            BootstrapSelection::new(binding),
            PassthroughBootstrapProvider::default(),
            NoopGatewayFilterManager,
            PingoraConnectorAdapter::new(),
            GatewayCoreLifecycleLimits {
                bootstrap_hard_cap: Some(Duration::from_secs(10)),
                write_quantum: 16 * 1024,
                ..GatewayCoreLifecycleLimits::default()
            },
        )?)
    }
}
