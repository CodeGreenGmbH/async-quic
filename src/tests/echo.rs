use crate::{ tests::{Tasks, server, quinn_client, sleep, handle, client, connect}};
use futures::{prelude::*, future::poll_fn};
use smol::{block_on, spawn};
use std::{
    net::{Ipv6Addr},
};
use test_log::test;
use h3::quic::*;

#[test]
fn echo_server() {
    let (mut server, port) = server();
    let handle_fut = handle(server, |conn| async {
        let stream = poll_fn(|cx| conn.poll_open_bidi(cx)).await.unwrap();
        let mut data = Vec::new();
        while let Some(b) = poll_fn(|cx| stream.poll_data(cx)).await.unwrap() {
            data.extend_from_slice(&b);
        }
        poll_fn(|cx| stream.poll_ready(cx)).await.unwrap();
        stream.send_data().unwrap();
        poll_fn(|cx| stream.poll_finish(cx)).await.unwrap();
    });

    const MSG: &'static [u8] = b"abcdefghijklmnop";

    let client = client();
    let data = connect(client, port, |conn| async {
        let stream = poll_fn(|cx| conn.poll_accept_bidi(cx)).await.unwrap();
        poll_fn(|cx| stream.poll_ready(cx)).await.unwrap();
        stream.send_data(MSG).unwrap();
        poll_fn(|cx| stream.poll_finish(cx)).await.unwrap();

        let mut data = Vec::new();
        while let Some(b) = poll_fn(|cx| stream.poll_data(cx)).await.unwrap() {
            data.extend_from_slice(&b);
        }
        data
    });

    assert_eq!(MSG, data);
}
