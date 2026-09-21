//! Bound line allocation before SDK parsing. SDK ByteStreams itself has unbounded lines.
use agent_client_protocol::{Client, ConnectTo, Lines};
use futures::StreamExt;
use std::io;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::codec::{FramedRead, LinesCodec};

pub const MAX_ACP_FRAME_BYTES: usize = 1024 * 1024;

pub fn bounded_transport<R, W>(reader: R, writer: W) -> impl ConnectTo<Client>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let incoming = FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_ACP_FRAME_BYTES))
        .map(|line| line.map_err(io::Error::other));
    let outgoing = futures::sink::unfold(writer, |mut writer, line: String| async move {
        if line.len() > MAX_ACP_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ACP frame too large",
            ));
        }
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok::<_, io::Error>(writer)
    });
    Lines::new(Box::pin(outgoing), incoming)
}
