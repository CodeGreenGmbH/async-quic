use crate::{QuicConnection, QuicEndpoint, QuicStream};
use std::{future::poll_fn, ops::ControlFlow};
use test_log::test;

use crate::tests::*;
use bytes::{Buf, Bytes};
use futures::{future::FusedFuture, join, prelude::*, select, stream::FuturesUnordered};
use http::{Method, Request, Response, Version};
use smol::block_on;

#[test]
fn h3_client_shutdown() {
    let (server, port) = server();
    let server_fut = handle(server, |conn| async {
        let mut conn = h3::server::Connection::new(conn).await.unwrap();
        assert!(conn.accept().await.unwrap().is_none());
        log::info!("Server: connection closed");
        ControlFlow::Break(())
    });

    let client = client();
    let client_fut = connect(client, port, |conn| async {
        let (mut h3_conn, sender) = h3::client::new(conn).await.unwrap();
        h3_conn.shutdown(0).await.unwrap();
        log::info!("Client: shutdown");
        poll_fn(|cx| h3_conn.poll_close(cx)).await.unwrap();
        log::info!("Client: connection closed");
        drop((h3_conn, sender))
    });

    block_on(async { join!(server_fut, client_fut) });
}

#[test]
fn h3_server_shutdown() {
    let (server, port) = server();
    let server_fut = handle(server, |conn| async {
        let mut conn = h3::server::Connection::new(conn).await.unwrap();
        conn.shutdown(0).await.unwrap();
        log::info!("Server: shutdown");
        assert!(conn.accept().await.unwrap().is_none());
        log::info!("Server: connection closed");
        ControlFlow::Break(())
    });

    let client = client();
    let client_fut = connect(client, port, |conn| async {
        let (mut h3_conn, sender) = h3::client::new(conn).await.unwrap();
        poll_fn(|cx| h3_conn.poll_close(cx)).await.unwrap();
        log::info!("Client: connection closed");
        drop((h3_conn, sender))
    });

    block_on(async { join!(server_fut, client_fut) });
}

pub async fn h3_connect<F, T, R>(ep: QuicEndpoint, port: u16, f: F) -> R
where
    F: FnOnce(h3::client::SendRequest<QuicConnection, Bytes>) -> T,
    T: Future<Output = R> + Unpin + FusedFuture,
{
    connect(ep, port, |conn| async {
        let (mut h3_conn, sender) = h3::client::new(conn).await.unwrap();
        let mut driver = poll_fn(|cx| h3_conn.poll_close(cx).map(|r| r.unwrap())).fuse();
        let mut fut = f(sender).fuse();
        let ret = select! {
            _ = driver => panic!("unexpected h3 conn close"),
            r = fut => r,
        };
        drop(driver);
        h3_conn.shutdown(0).await.unwrap();
        poll_fn(|cx| h3_conn.poll_close(cx)).await.unwrap();
        ret
    })
    .await
}

pub async fn h3_handle<F, T>(ep: QuicEndpoint, f: F)
where
    F: Fn(http::Request<()>, h3::server::RequestStream<QuicStream<true, true>, Bytes>) -> T,
    T: Future,
{
    handle(ep, |conn| async {
        let mut conn = h3::server::Connection::new(conn).await.unwrap();
        let mut handling = FuturesUnordered::new();
        let mut accept_fut = Box::pin(conn.accept().fuse());
        loop {
            select! {
                a = accept_fut => match a.unwrap() {
                    Some((req, stream)) => handling.push(f(req, stream)),
                    None => {},
                },
                _ = handling.next() => {},
                complete => break
            }
        }
        drop(());
        ControlFlow::Break(())
    })
    .await
}

#[test]
fn h3_echo() {
    let (server, port) = server();
    let resp_fut = h3_handle(server, |_, mut stream| {
        Box::pin(async move {
            let mut body = Vec::new();
            while let Some(b) = stream.recv_data().await.unwrap() {
                body.extend_from_slice(b.chunk())
            }
            let resp = Response::new(());
            stream.send_response(resp).await.unwrap();
            stream.send_data(body.into()).await.unwrap();
            stream.finish().await.unwrap();
        })
        .fuse()
    });
    let resp_fut = Box::pin(resp_fut).fuse();

    const MSG: &'static [u8] = b"abcdefghijklmnop";

    let client = client();
    let req_fut = Box::pin(h3_connect(client, port, |mut sender| {
        Box::pin(async move {
            let req = Request::builder()
                .uri("https://localhost")
                .method(Method::POST)
                .version(Version::HTTP_3)
                .body(())
                .unwrap();
            let mut stream = sender.send_request(req).await.unwrap();
            stream.send_data(Bytes::copy_from_slice(MSG)).await.unwrap();
            stream.finish().await.unwrap();
            stream.recv_response().await.unwrap();
            let mut resp_body = Vec::new();
            while let Some(b) = stream.recv_data().await.unwrap() {
                resp_body.extend_from_slice(b.chunk())
            }
            resp_body
        })
        .fuse()
    }));
    let req_fut = Box::pin(req_fut).fuse();

    let ((), data) = block_on(async { join!(resp_fut, req_fut) });

    assert_eq!(MSG, data);
}
