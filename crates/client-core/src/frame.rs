use crate::FailureCode;
use hiroute_application_api::LOCAL_CONTROL_MAX_FRAME_BYTES;
use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use zeroize::Zeroizing;

struct BoundedFrame(Zeroizing<Vec<u8>>);
impl std::io::Write for BoundedFrame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) >= LOCAL_CONTROL_MAX_FRAME_BYTES {
            return Err(std::io::Error::other("frame bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn encode(value: &impl Serialize) -> Result<Zeroizing<Vec<u8>>, FailureCode> {
    let mut buffer = BoundedFrame(Zeroizing::new(Vec::new()));
    serde_json::to_writer(&mut buffer, value).map_err(|error| {
        if error.is_io() {
            FailureCode::FrameTooLarge
        } else {
            FailureCode::FrameInvalid
        }
    })?;
    buffer.0.push(b'\n');
    Ok(buffer.0)
}

pub(crate) async fn read(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Zeroizing<Vec<u8>>, FailureCode> {
    let mut frame = Zeroizing::new(Vec::new());
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| FailureCode::TransportUnavailable)?;
        if available.is_empty() {
            return Err(FailureCode::FrameInvalid);
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |n| n + 1);
        if frame.len().saturating_add(count) > LOCAL_CONTROL_MAX_FRAME_BYTES {
            return Err(FailureCode::FrameTooLarge);
        }
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if frame.last() == Some(&b'\n') {
            frame.pop();
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            if frame.is_empty() {
                return Err(FailureCode::FrameInvalid);
            }
            return Ok(frame);
        }
    }
}
