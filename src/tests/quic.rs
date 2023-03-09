use std::{future::poll_fn, ops::ControlFlow};

use crate::tests::{client, connect, handle, server};
use futures::{join, prelude::*};
use h3::quic::*;
use smol::block_on;
use test_log::test;

#[test]
fn uni_stream() {
    let (server, port) = server();
    let recv_fut = handle(server, |mut conn| {
        Box::pin(async move {
            let mut recv_stream = poll_fn(|cx| conn.poll_accept_recv(cx)).await.unwrap().unwrap();
            let mut content = Vec::new();
            recv_stream.read_to_end(&mut content).await.unwrap();
            return ControlFlow::Break(content);
        })
        .fuse()
    });
    let recv_fut = Box::pin(recv_fut).fuse();

    const MSG: &'static [u8] = b"abcdefghijklmnop";

    let client = client();
    let send_fut = connect(client, port, |mut conn| {
        Box::pin(async move {
            let mut send_stream = poll_fn(|cx| conn.poll_open_send(cx)).await.unwrap();
            send_stream.write_all(MSG).await.unwrap();
            send_stream.close().await.unwrap();
            conn.close(h3::error::Code::H3_NO_ERROR, &[]);
        })
        .fuse()
    });
    let send_fut = Box::pin(send_fut).fuse();

    let (data, ()) = block_on(async { join!(recv_fut, send_fut) });
    assert_eq!(MSG, data);
}

#[test]
fn many_streams() {
    let (server, port) = server();
    const COUNT: usize = 10;
    let mut counter = 0usize;
    let recv_fut = handle(server, |mut conn| {
        Box::pin(async move {
            let mut control = ControlFlow::Continue(());
            while let Some(mut recv_stream) = poll_fn(|cx| conn.poll_accept_bidi(cx)).await.unwrap() {
                let n = recv_stream.read(&mut [0u8; 1]).await.unwrap();
                assert_eq!(n, 0);
                counter += 1;
                if counter == COUNT {
                    control = ControlFlow::Break(counter);
                }
            }
            control
        })
        .fuse()
    });
    let recv_fut = Box::pin(recv_fut).fuse();

    let client = client();
    let send_fut = connect(client, port, |mut conn| {
        Box::pin(async move {
            let mut streams = Vec::new();
            for _ in 0..COUNT {
                streams.push(poll_fn(|cx| conn.poll_open_bidi(cx)).await.unwrap());
            }
            for mut stream in streams.into_iter() {
                stream.close().await.unwrap();
            }
            conn.close(h3::error::Code::H3_NO_ERROR, &[]);
        })
        .fuse()
    });
    let send_fut = Box::pin(send_fut).fuse();

    let (counter, ()) = block_on(async { join!(recv_fut, send_fut) });
    assert_eq!(counter, COUNT);
}
