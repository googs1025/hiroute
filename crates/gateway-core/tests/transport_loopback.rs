use std::error::Error;
use std::future::poll_fn;
use std::io;
use std::time::Duration;

use bytes::Bytes;
use h2::client;
use http::{Request, Response, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

type TestError = Box<dyn Error + Send + Sync>;

const H1_RESPONSE: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok";

async fn read_h1_head(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(512);
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await?;
        head.push(byte[0]);
        if head.len() > 8 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head exceeds smoke limit",
            ));
        }
    }
    Ok(head)
}

#[tokio::test]
async fn loopback_plain_http_smoke_reuses_h1_connection_and_joins_server() -> Result<(), TestError>
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        for expected in [b"/first".as_slice(), b"/second".as_slice()] {
            let head = read_h1_head(&mut socket).await?;
            if !head
                .windows(expected.len())
                .any(|window| window == expected)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected request target",
                ));
            }
            socket.write_all(H1_RESPONSE).await?;
        }
        socket.shutdown().await
    });

    let mut client = TcpStream::connect(address).await?;
    for path in ["/first", "/second"] {
        client
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: loopback.test\r\n\r\n").as_bytes())
            .await?;
        let mut response = vec![0_u8; H1_RESPONSE.len()];
        client.read_exact(&mut response).await?;
        assert_eq!(response, H1_RESPONSE);
    }
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server).await???;
    Ok(())
}

#[tokio::test]
async fn disconnected_h1_request_body_is_detected_without_detached_work() -> Result<(), TestError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let _head = read_h1_head(&mut socket).await?;
        let mut body = [0_u8; 32];
        Ok::<bool, io::Error>(socket.read_exact(&mut body).await.is_err())
    });

    let mut client = TcpStream::connect(address).await?;
    client
        .write_all(
            b"POST /partial HTTP/1.1\r\nHost: loopback.test\r\nContent-Length: 32\r\n\r\nabc",
        )
        .await?;
    client.shutdown().await?;
    drop(client);
    let disconnected = tokio::time::timeout(Duration::from_secs(2), server).await???;
    assert!(disconnected);
    Ok(())
}

#[tokio::test]
async fn real_h2_listener_observes_slow_consumer_flow_control_and_joins() -> Result<(), TestError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (blocked_tx, blocked_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await?;
        let mut connection = h2::server::handshake(socket).await?;
        let (request, mut respond) = connection
            .accept()
            .await
            .ok_or("client closed before request")??;
        if request.uri().path() != "/events" {
            return Err::<(), TestError>("unexpected H2 request path".into());
        }
        let mut handler = Box::pin(async move {
            let response = Response::builder().status(StatusCode::OK).body(())?;
            let mut body = respond.send_response(response, false)?;
            body.send_data(Bytes::from_static(b"12345678"), false)?;
            body.reserve_capacity(8);
            let was_blocked = tokio::time::timeout(
                Duration::from_millis(50),
                poll_fn(|cx| body.poll_capacity(cx)),
            )
            .await
            .is_err();
            let _ = blocked_tx.send(was_blocked);
            let granted = poll_fn(|cx| body.poll_capacity(cx))
                .await
                .ok_or("H2 stream closed while waiting for capacity")??;
            if granted < 8 {
                return Err::<(), TestError>("insufficient H2 send capacity".into());
            }
            body.send_data(Bytes::from_static(b"abcdefgh"), true)?;
            Ok::<(), TestError>(())
        });
        let handler_result = tokio::select! {
            result = &mut handler => result,
            next = connection.accept() => match next {
                Some(Ok(_)) => return Err::<(), TestError>("unexpected second H2 request".into()),
                Some(Err(error)) => return Err::<(), TestError>(error.into()),
                None => return Err::<(), TestError>("H2 connection closed before response".into()),
            },
        };
        handler_result?;
        connection.graceful_shutdown();
        if let Some(next) = connection.accept().await {
            next?;
            return Err::<(), TestError>("unexpected H2 request during shutdown".into());
        }
        Ok::<(), TestError>(())
    });

    let socket = TcpStream::connect(address).await?;
    let mut builder = client::Builder::new();
    builder.initial_window_size(8);
    let (sender, connection) = builder.handshake::<_, Bytes>(socket).await?;
    let client_driver = tokio::spawn(connection);
    let mut sender = sender.ready().await?;
    let request = Request::builder()
        .uri("http://loopback.test/events")
        .body(())?;
    let (response, _) = sender.send_request(request, true)?;
    let response = response.await?;
    assert_eq!(response.status(), StatusCode::OK);
    let mut recv = response.into_body();
    let first = recv.data().await.ok_or("missing first H2 data")??;
    assert_eq!(first, Bytes::from_static(b"12345678"));
    assert!(
        blocked_rx.await?,
        "server should stop on exhausted H2 window"
    );
    recv.flow_control().release_capacity(first.len())?;
    let second = recv.data().await.ok_or("missing second H2 data")??;
    assert_eq!(second, Bytes::from_static(b"abcdefgh"));
    recv.flow_control().release_capacity(second.len())?;
    assert!(recv.data().await.is_none());

    drop(recv);
    drop(sender);
    tokio::time::timeout(Duration::from_secs(2), server).await???;
    tokio::time::timeout(Duration::from_secs(2), client_driver).await???;
    Ok(())
}
