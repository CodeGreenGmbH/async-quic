use std::{future::poll_fn, ops::ControlFlow};

use crate::{tests::handle, QuicConnection, QuicEndpoint, QuicStream};

use super::{client, connect, server};
use bytes::{Buf, Bytes};
use futures::{future::FusedFuture, prelude::*, select, stream::FuturesUnordered};
use http::{Request, Response};
use smol::block_on;

pub async fn h3_connect<F, T, R>(ep: QuicEndpoint, port: u16, f: F) -> R
where
    F: FnOnce(h3::client::SendRequest<QuicConnection, Bytes>) -> T,
    T: Future<Output = R> + Unpin + FusedFuture,
{
    connect(ep, port, |conn| {
        Box::pin(async {
            let (mut driver, sender) = h3::client::new(conn).await.unwrap();
            let mut driver = poll_fn(move |cx| driver.poll_close(cx).map(|r| r.unwrap())).fuse();
            let mut fut = f(sender).fuse();
            select! {
                _ = driver => panic!("unexpeced connection close"),
               r = fut => r,
            }
        })
        .fuse()
    })
    .await
}

pub async fn h3_handle<F, T, R>(ep: QuicEndpoint, f: F) -> R
where
    F: Fn(http::Request<()>, h3::server::RequestStream<QuicStream<true, true>, Bytes>) -> T,
    T: Future<Output = ControlFlow<R>>,
{
    handle(ep, |conn| async {
        let mut conn = h3::server::Connection::new(conn).await.unwrap();
        let mut handling = FuturesUnordered::new();
        loop {
            if let ControlFlow::Break(r) = select! {
            a = conn.accept().fuse() => {
                let (req, stream) = a.unwrap().unwrap();
                handling.push(f(req, stream));
                ControlFlow::Continue(())
            },
                r = handling.next() => r.unwrap(),
            } {
                return ControlFlow::Break(r);
            }
        }
    })
    .await
}

#[test]
fn echo_server() {
    let (server, port) = server();
    let handle_fut = h3_handle(server, |_, mut stream| {
        Box::pin(async move {
            let mut body = Vec::new();
            while let Some(b) = stream.recv_data().await.unwrap() {
                body.extend_from_slice(b.chunk())
            }
            let resp = Response::new(());
            stream.send_response(resp).await.unwrap();
            stream.send_data(body.into()).await.unwrap();
            stream.finish().await.unwrap();
            ControlFlow::<()>::Continue(())
        })
        .fuse()
    });
    let mut handle_fut = Box::pin(handle_fut).fuse();

    const MSG: &'static [u8] = b"abcdefghijklmnop";

    let client = client();
    let request_fut = Box::pin(h3_connect(client, port, |mut sender| {
        Box::pin(async move {
            let req = Request::new(());
            let mut stream = sender.send_request(req).await.unwrap();
            stream.send_data(Bytes::copy_from_slice(MSG)).await.unwrap();
            stream.recv_response().await.unwrap();
            let mut resp_body = Vec::new();
            while let Some(b) = stream.recv_data().await.unwrap() {
                resp_body.extend_from_slice(b.chunk())
            }
            resp_body
        })
        .fuse()
    }));
    let mut request_fut = Box::pin(request_fut).fuse();

    let data = block_on(async {
        select! {
            _ = handle_fut => panic!("handle finished"),
            data = request_fut => data,
        }
    });
    assert_eq!(MSG, data);
}

//#[test]
//fn h3_echo_server() {
//    let (server_ep, port) = server();
//    let client_ep = client();
//
//    block_on(async move {
//        join!(
//            handle_conn(server_ep),
//            init_conn(client_ep, port)
//        )
//    });
//}
//
//async fn handle_conn(mut ep: QuicEndpoint) {
//    let ep = ep.fuse();
//    let mut conn = h3::server::Connection::new(conn).await.unwrap();
//    let mut connecting = FuturesUnordered::new();
//    let mut accepting = FuturesUnordered::new();
//    let mut handling = FuturesUnordered::new();
//    loop {
//        select!{
//            conn = ep.next().fuse() => {
//                let conn = conn.unwrap();
//
//            },
//            _ = handling.next() => {},
//            req_stream = conn.accept().fuse() => {
//                let (req, stream) = req_stream.unwrap().unwrap();
//                handling.push(handle_request(req, stream))
//            }
//        }
//    }
//}

//
//async fn init_conn(mut ep: QuicEndpoint, port: u16) {
//    let conn = ep.connect(Arc::new(RUSTLS_CLIENT_CONFIG.clone()), (Ipv6Addr::LOCALHOST, port).into(), "localhost").unwrap();
//    let conn = conn.await.unwrap();
//    let (mut driver, sender) = h3::client::new(conn).await.unwrap();
//    let mut conn_drive = poll_fn(move |cx| driver.poll_close(cx).map(|r| r.unwrap())).fuse();
//    let mut send_drive = Box::pin(send_request(sender)).fuse();
//        select! {
//            _ = ep.next().fuse() => panic!("unexpeced ep event"),
//            _ = conn_drive => panic!("connection closed"),
//            _ = send_drive => {}
//        }
//}
//
//async fn send_request<T: h3::quic::OpenStreams<Bytes>>(mut send_request: h3::client::SendRequest<T, Bytes>) {
//    let req = Request::new(());
//    let mut stream = send_request.send_request(req).await.unwrap();
//    let mut rng = thread_rng();
//    let mut req_body = vec![0u8; rng.gen()];
//    rng.fill(req_body.as_mut_slice());
//    stream.send_data(Bytes::copy_from_slice(&req_body)).await.unwrap();
//    stream.recv_response().await.unwrap();
//    let mut resp_body = Vec::new();
//    while let Some(b) = stream.recv_data().await.unwrap() {
//        resp_body.extend_from_slice(b.chunk())
//    }
//    assert_eq!(req_body, resp_body)
//}
//
//async fn handle_request<S: h3::quic::BidiStream<Bytes>>(_req: Request<()>, mut stream: h3::server::RequestStream<S, Bytes>) {
//    let mut body = Vec::new();
//    while let Some(b) = stream.recv_data().await.unwrap() {
//        body.extend_from_slice(b.chunk())
//    }
//    dbg!(&body);
//    let resp = Response::new(());
//    stream.send_response(resp).await.unwrap();
//    stream.send_data(body.into()).await.unwrap();
//    stream.finish().await.unwrap();
//}
