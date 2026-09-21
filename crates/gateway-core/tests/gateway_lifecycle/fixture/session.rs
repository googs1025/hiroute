use super::*;

#[derive(Default)]
pub(crate) struct ShutdownLifecycleFacts {
    pub(crate) entered: Notify,
    pub(crate) cancellation_seen: AtomicUsize,
    pub(crate) process_dropped: AtomicUsize,
    pub(crate) dropped: Notify,
}

pub(crate) struct ShutdownProcessGuard(Arc<ShutdownLifecycleFacts>);

impl Drop for ShutdownProcessGuard {
    fn drop(&mut self) {
        self.0.process_dropped.fetch_add(1, Ordering::Relaxed);
        self.0.dropped.notify_one();
    }
}

#[derive(Clone)]
pub(crate) struct ShutdownHangingLifecycle {
    pub(crate) facts: Arc<ShutdownLifecycleFacts>,
}

#[async_trait]
impl GatewayLifecycle for ShutdownHangingLifecycle {
    async fn process(
        &self,
        session: &mut dyn GatewaySession,
    ) -> Result<SessionReuse, TransportError> {
        let _guard = ShutdownProcessGuard(Arc::clone(&self.facts));
        let cancellation = session.cancellation_token();
        self.facts.entered.notify_one();
        cancellation.cancelled().await;
        self.facts.cancellation_seen.fetch_add(1, Ordering::Relaxed);
        std::future::pending().await
    }
}

#[derive(Default)]
pub(crate) struct RecordingFailingSink {
    pub(crate) events: Mutex<Vec<LifecycleEvent>>,
}

impl ObservationSink for RecordingFailingSink {
    fn try_emit(&self, event: LifecycleEvent) -> Result<(), ObservationError> {
        self.events.lock().expect("telemetry events").push(event);
        Err(ObservationError::Unavailable)
    }
}
#[derive(Debug)]
pub(crate) struct RecordingSession {
    pub(crate) request_head: GatewayRequestHead,
    pub(crate) request_body: VecDeque<Bytes>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) response_head: Option<GatewayResponseHead>,
    pub(crate) response_body: Vec<u8>,
    pub(crate) response_body_writes: Vec<(Bytes, bool)>,
    pub(crate) response_eos: bool,
    pub(crate) request_body_delay: Option<Duration>,
    pub(crate) response_head_delay: Option<Duration>,
    pub(crate) request_body_reads: Arc<AtomicUsize>,
    pub(crate) response_head_writes: Arc<AtomicUsize>,
    pub(crate) observed_response_body_writes: Arc<Mutex<Vec<(Bytes, bool)>>>,
    pub(crate) response_body_write_observed: Arc<Notify>,
}

impl RecordingSession {
    pub(crate) fn new(host: &str, path: &str, body: Bytes, protocol: HttpProtocol) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_str(host).expect("test host"));
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&body.len().to_string()).expect("test length"),
        );
        Self {
            request_head: GatewayRequestHead {
                method: Method::POST,
                path_and_query: path.into(),
                authority: None,
                headers,
                protocol,
            },
            request_body: VecDeque::from([body]),
            cancellation: CancellationToken::new(),
            response_head: None,
            response_body: Vec::new(),
            response_body_writes: Vec::new(),
            response_eos: false,
            request_body_delay: None,
            response_head_delay: None,
            request_body_reads: Arc::new(AtomicUsize::new(0)),
            response_head_writes: Arc::new(AtomicUsize::new(0)),
            observed_response_body_writes: Arc::new(Mutex::new(Vec::new())),
            response_body_write_observed: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn cancellation_handle(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn with_method(mut self, method: Method) -> Self {
        self.request_head.method = method;
        self
    }

    pub(crate) fn with_response_head_delay(mut self, delay: Duration) -> Self {
        self.response_head_delay = Some(delay);
        self
    }

    pub(crate) fn with_request_body_delay(mut self, delay: Duration) -> Self {
        self.request_body_delay = Some(delay);
        self
    }

    pub(crate) fn with_body_chunks(mut self, chunks: impl IntoIterator<Item = Bytes>) -> Self {
        self.request_body = chunks.into_iter().collect();
        let length: usize = self.request_body.iter().map(Bytes::len).sum();
        self.request_head.headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).expect("body length"),
        );
        self
    }
}

#[async_trait]
impl GatewaySession for RecordingSession {
    fn request_head(&self) -> Result<GatewayRequestHead, TransportError> {
        Ok(self.request_head.clone())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    async fn wait_for_disconnect(&mut self) {
        self.cancellation.cancelled().await;
    }

    async fn read_request_body(&mut self) -> Result<Option<Bytes>, TransportError> {
        self.request_body_reads.fetch_add(1, Ordering::Relaxed);
        if let Some(delay) = self.request_body_delay {
            tokio::time::sleep(delay).await;
        }
        Ok(self.request_body.pop_front())
    }

    async fn write_response_head(
        &mut self,
        head: GatewayResponseHead,
    ) -> Result<(), TransportError> {
        if let Some(delay) = self.response_head_delay {
            tokio::time::sleep(delay).await;
        }
        if self.response_head.replace(head).is_some() {
            return Err(TransportError::Io("duplicate response head".into()));
        }
        self.response_head_writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn write_response_body(
        &mut self,
        body: Bytes,
        end_stream: bool,
    ) -> Result<(), TransportError> {
        self.response_body.extend_from_slice(&body);
        self.response_body_writes.push((body, end_stream));
        self.observed_response_body_writes
            .lock()
            .expect("observed response body writes")
            .push((
                self.response_body_writes
                    .last()
                    .expect("just pushed")
                    .0
                    .clone(),
                end_stream,
            ));
        self.response_body_write_observed.notify_waiters();
        self.response_eos |= end_stream;
        Ok(())
    }
}
