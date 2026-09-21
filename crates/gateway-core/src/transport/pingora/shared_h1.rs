use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SharedH1Role {
    Reader,
    Writer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum SharedH1Mode {
    Setup = 0,
    Split = 1,
    Unified = 2,
}

/// Two Pingora H1 codec owners use Tokio's read/write halves of one
/// already-established stream. Setup writes are sinks used only to initialize
/// identical request framing state after the original session sent the one
/// real header. In Split mode exactly one owner holds each I/O half; Unified
/// is entered only after both normal EOS facts, before the reader session
/// returns to Pingora's keepalive pool.
pub(super) struct SharedH1Stream {
    pub(super) read_half: Mutex<Option<ReadHalf<Stream>>>,
    pub(super) write_half: Mutex<Option<WriteHalf<Stream>>>,
    pub(super) unified: Mutex<Option<Stream>>,
    pub(super) mode: AtomicU8,
    pub(super) digest: Digest,
    pub(super) id: UniqueIDType,
    pub(super) alpn: Option<ALPN>,
}

impl SharedH1Stream {
    pub(super) fn reunify(&self) -> io::Result<()> {
        if self.mode.load(Ordering::Acquire) == SharedH1Mode::Unified as u8 {
            return Ok(());
        }
        let read_half = self
            .read_half
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| io::Error::other("H1 read half is unavailable for reunification"))?;
        let write_half = self
            .write_half
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| io::Error::other("H1 write half is unavailable for reunification"))?;
        *self
            .unified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(read_half.unsplit(write_half));
        self.mode
            .store(SharedH1Mode::Unified as u8, Ordering::Release);
        Ok(())
    }
}

impl std::fmt::Debug for SharedH1Stream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedH1Stream")
            .field("mode", &self.mode.load(Ordering::Acquire))
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(super) struct SharedH1Io {
    pub(super) shared: Arc<SharedH1Stream>,
    pub(super) role: SharedH1Role,
}

impl std::fmt::Debug for SharedH1Io {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedH1Io")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl SharedH1Io {
    fn mode(&self) -> SharedH1Mode {
        match self.shared.mode.load(Ordering::Acquire) {
            0 => SharedH1Mode::Setup,
            1 => SharedH1Mode::Split,
            _ => SharedH1Mode::Unified,
        }
    }

    fn may_read(&self) -> bool {
        self.mode() == SharedH1Mode::Unified || self.role == SharedH1Role::Reader
    }

    fn may_write(&self) -> bool {
        match self.mode() {
            SharedH1Mode::Setup => false,
            SharedH1Mode::Split => self.role == SharedH1Role::Writer,
            SharedH1Mode::Unified => true,
        }
    }
}

/// Pingora pools the reader-side wrapper after a reusable H1 exchange. Peel
/// that wrapper before creating the next duplex pair so repeated keepalive
/// requests do not accumulate nested mutex/codec layers. A concurrently held
/// wrapper is conservatively preserved; normal pool release leaves exactly
/// one owner and therefore takes the zero-copy `Arc::try_unwrap` path.
pub(super) fn flatten_reused_h1_stream(stream: Stream) -> Stream {
    if !stream.as_any().is::<SharedH1Io>() {
        return stream;
    }
    let io = *stream
        .into_any()
        .downcast::<SharedH1Io>()
        .expect("type identity was checked before downcast");
    if io.role != SharedH1Role::Reader || io.mode() != SharedH1Mode::Unified {
        return Box::new(io);
    }
    if io
        .shared
        .unified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_none()
    {
        return Box::new(io);
    }
    match Arc::try_unwrap(io.shared) {
        Ok(shared) => shared
            .unified
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .expect("unique unified H1 wrapper was checked before extraction"),
        Err(shared) => Box::new(SharedH1Io {
            shared,
            role: SharedH1Role::Reader,
        }),
    }
}

impl AsyncRead for SharedH1Io {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.may_read() {
            return Poll::Pending;
        }
        match self.mode() {
            SharedH1Mode::Setup | SharedH1Mode::Split => {
                let mut read_half = self
                    .shared
                    .read_half
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(read_half) = read_half.as_mut() else {
                    return Poll::Ready(Err(io::Error::other("H1 read half is unavailable")));
                };
                Pin::new(read_half).poll_read(context, buffer)
            }
            SharedH1Mode::Unified => {
                let mut stream = self
                    .shared
                    .unified
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(stream) = stream.as_mut() else {
                    return Poll::Ready(Err(io::Error::other("unified H1 stream is unavailable")));
                };
                Pin::new(stream.as_mut()).poll_read(context, buffer)
            }
        }
    }
}

impl AsyncWrite for SharedH1Io {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.mode() == SharedH1Mode::Setup {
            return Poll::Ready(Ok(buffer.len()));
        }
        if !self.may_write() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "H1 response owner cannot write while the duplex writer is live",
            )));
        }
        match self.mode() {
            SharedH1Mode::Setup => Poll::Ready(Ok(buffer.len())),
            SharedH1Mode::Split => {
                let mut write_half = self
                    .shared
                    .write_half
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(write_half) = write_half.as_mut() else {
                    return Poll::Ready(Err(io::Error::other("H1 write half is unavailable")));
                };
                Pin::new(write_half).poll_write(context, buffer)
            }
            SharedH1Mode::Unified => {
                let mut stream = self
                    .shared
                    .unified
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(stream) = stream.as_mut() else {
                    return Poll::Ready(Err(io::Error::other("unified H1 stream is unavailable")));
                };
                Pin::new(stream.as_mut()).poll_write(context, buffer)
            }
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.mode() == SharedH1Mode::Setup {
            return Poll::Ready(Ok(buffers.iter().map(|buffer| buffer.len()).sum()));
        }
        if !self.may_write() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "H1 response owner cannot write while the duplex writer is live",
            )));
        }
        match self.mode() {
            SharedH1Mode::Setup => Poll::Ready(Ok(buffers.iter().map(|buffer| buffer.len()).sum())),
            SharedH1Mode::Split => {
                let mut write_half = self
                    .shared
                    .write_half
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(write_half) = write_half.as_mut() else {
                    return Poll::Ready(Err(io::Error::other("H1 write half is unavailable")));
                };
                Pin::new(write_half).poll_write_vectored(context, buffers)
            }
            SharedH1Mode::Unified => {
                let mut stream = self
                    .shared
                    .unified
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(stream) = stream.as_mut() else {
                    return Poll::Ready(Err(io::Error::other("unified H1 stream is unavailable")));
                };
                Pin::new(stream.as_mut()).poll_write_vectored(context, buffers)
            }
        }
    }

    fn is_write_vectored(&self) -> bool {
        true
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.mode() == SharedH1Mode::Setup {
            return Poll::Ready(Ok(()));
        }
        if !self.may_write() {
            return Poll::Ready(Ok(()));
        }
        match self.mode() {
            SharedH1Mode::Setup => Poll::Ready(Ok(())),
            SharedH1Mode::Split => {
                let mut write_half = self
                    .shared
                    .write_half
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(write_half) = write_half.as_mut() else {
                    return Poll::Ready(Err(io::Error::other("H1 write half is unavailable")));
                };
                Pin::new(write_half).poll_flush(context)
            }
            SharedH1Mode::Unified => {
                let mut stream = self
                    .shared
                    .unified
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let Some(stream) = stream.as_mut() else {
                    return Poll::Ready(Err(io::Error::other("unified H1 stream is unavailable")));
                };
                Pin::new(stream.as_mut()).poll_flush(context)
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Err(error) = self.shared.reunify() {
            return Poll::Ready(Err(error));
        }
        let mut stream = self
            .shared
            .unified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(stream) = stream.as_mut() else {
            return Poll::Ready(Err(io::Error::other("unified H1 stream is unavailable")));
        };
        Pin::new(stream.as_mut()).poll_shutdown(context)
    }
}

#[async_trait]
impl Shutdown for SharedH1Io {
    async fn shutdown(&mut self) {
        let _ = std::future::poll_fn(|context| Pin::new(&mut *self).poll_shutdown(context)).await;
    }
}

impl UniqueID for SharedH1Io {
    fn id(&self) -> UniqueIDType {
        self.shared.id
    }
}

impl Ssl for SharedH1Io {
    fn get_ssl_digest(&self) -> Option<Arc<pingora_core::protocols::tls::SslDigest>> {
        self.shared.digest.ssl_digest.clone()
    }

    fn selected_alpn_proto(&self) -> Option<ALPN> {
        self.shared.alpn.clone()
    }
}

impl GetTimingDigest for SharedH1Io {
    fn get_timing_digest(&self) -> Vec<Option<pingora_core::protocols::TimingDigest>> {
        self.shared.digest.timing_digest.clone()
    }
}

impl GetProxyDigest for SharedH1Io {
    fn get_proxy_digest(&self) -> Option<Arc<pingora_core::protocols::raw_connect::ProxyDigest>> {
        self.shared.digest.proxy_digest.clone()
    }
}

impl GetSocketDigest for SharedH1Io {
    fn get_socket_digest(&self) -> Option<Arc<pingora_core::protocols::SocketDigest>> {
        self.shared.digest.socket_digest.clone()
    }
}

#[async_trait]
impl Peek for SharedH1Io {}
