extern crate std;

use core::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
use std::{boxed::Box, vec::Vec};

use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use crate::{
    CorrelationId, DeviceApiHandoff, LocalApiReply, LocalApiRequest, MessageLength, OwnedMessage,
    RequestKey, SessionEpoch,
};

struct TestGrant {
    principal: [u8; 16],
    permissions: u32,
    mint_nonce: u64,
}

fn grant(value: u8, permissions: u32, mint_nonce: u64) -> TestGrant {
    TestGrant {
        principal: [value; 16],
        permissions,
        mint_nonce,
    }
}

fn key(epoch: u64, correlation: u64) -> RequestKey {
    RequestKey::new(SessionEpoch::new(epoch), CorrelationId::new(correlation))
}

fn patterned_buffer(seed: u8) -> [u8; 512] {
    let mut buffer = [0_u8; 512];
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = seed.wrapping_add((index as u8).wrapping_mul(29));
    }
    buffer
}

fn message(seed: u8, length: usize) -> OwnedMessage {
    OwnedMessage::new(
        MessageLength::new(length).expect("test message must fit"),
        patterned_buffer(seed),
    )
}

fn request(
    epoch: u64,
    correlation: u64,
    grant_value: u8,
    permissions: u32,
    mint_nonce: u64,
    seed: u8,
    length: usize,
) -> LocalApiRequest<TestGrant> {
    LocalApiRequest::new(
        key(epoch, correlation),
        grant(grant_value, permissions, mint_nonce),
        message(seed, length),
    )
}

fn reply(epoch: u64, correlation: u64, seed: u8, length: usize) -> LocalApiReply {
    LocalApiReply::new(key(epoch, correlation), message(seed, length))
}

fn handoff() -> &'static mut DeviceApiHandoff<NoopRawMutex, TestGrant> {
    Box::leak(Box::new(DeviceApiHandoff::new()))
}

fn poll_once<F>(future: core::pin::Pin<&mut F>) -> Poll<F::Output>
where
    F: Future,
{
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    future.poll(&mut context)
}

fn assert_request(
    request: &LocalApiRequest<TestGrant>,
    expected_key: RequestKey,
    grant_value: u8,
    permissions: u32,
    mint_nonce: u64,
    seed: u8,
    length: usize,
) {
    assert_eq!(request.key(), expected_key);
    assert_eq!(request.grant().principal, [grant_value; 16]);
    assert_eq!(request.grant().permissions, permissions);
    assert_eq!(request.grant().mint_nonce, mint_nonce);
    assert_eq!(request.message().length().get(), length);
    assert_eq!(request.message().full_buffer(), &patterned_buffer(seed));
    assert_eq!(
        request.message().encoded(),
        &patterned_buffer(seed)[..length]
    );
}

fn assert_reply(reply: &LocalApiReply, expected_key: RequestKey, seed: u8, length: usize) {
    assert_eq!(reply.key(), expected_key);
    assert_eq!(reply.message().length().get(), length);
    assert_eq!(reply.message().full_buffer(), &patterned_buffer(seed));
    assert_eq!(reply.message().encoded(), &patterned_buffer(seed)[..length]);
}

#[test]
fn cancel_before_enqueue_retains_exact_request_owner_and_cancelled_receive_loses_nothing() {
    let (mut bearer, mut node) = handoff().split();

    {
        let mut cancelled_receive = pin!(node.requests().receive());
        assert!(matches!(
            poll_once(cancelled_receive.as_mut()),
            Poll::Pending
        ));
    }

    bearer
        .requests()
        .try_send(request(1, 10, 0x11, 0x03, 91, 0x20, 17))
        .unwrap_or_else(|_| panic!("empty request channel rejected first owner"));

    let second = request(1, 11, 0x22, 0x05, 92, 0x30, 511);
    {
        let mut cancelled_capacity = pin!(bearer.requests().wait_ready_to_send());
        assert!(matches!(
            poll_once(cancelled_capacity.as_mut()),
            Poll::Pending
        ));
    }

    assert_request(&second, key(1, 11), 0x22, 0x05, 92, 0x30, 511);

    let first = node
        .requests()
        .try_receive()
        .expect("cancelled receive must leave queued request intact");
    assert_request(&first, key(1, 10), 0x11, 0x03, 91, 0x20, 17);

    bearer
        .requests()
        .try_send(second)
        .unwrap_or_else(|_| panic!("capacity cancellation reserved the channel"));
    let second = node
        .requests()
        .try_receive()
        .expect("retained second owner must enqueue unchanged");
    assert_request(&second, key(1, 11), 0x22, 0x05, 92, 0x30, 511);
}

#[test]
fn connection_epoch_change_cannot_revoke_node_acceptance_or_destroy_bearer_roles() {
    let (mut bearer, mut node) = handoff().split();
    bearer
        .requests()
        .try_send(request(7, 70, 0x44, 0x0f, 700, 0x51, 512))
        .unwrap_or_else(|_| panic!("empty request channel rejected owner"));

    let accepted = node
        .requests()
        .try_receive()
        .expect("session loss must not cancel an enqueued mutation");
    assert_request(&accepted, key(7, 70), 0x44, 0x0f, 700, 0x51, 512);

    node.replies()
        .try_send(reply(7, 70, 0x61, 31))
        .unwrap_or_else(|_| panic!("node must finish independently of connection lifetime"));

    let current_epoch = SessionEpoch::new(8);
    let stale = bearer
        .replies()
        .try_receive()
        .expect("boot-lifetime bearer manager must drain stale replies");
    assert!(!stale.belongs_to_epoch(current_epoch));

    bearer
        .requests()
        .try_send(request(8, 80, 0x44, 0x0f, 800, 0x52, 32))
        .unwrap_or_else(|_| panic!("new session must retain the same bearer capability"));
    let next = node
        .requests()
        .try_receive()
        .expect("new session request must reach the persistent node role");
    assert_request(&next, key(8, 80), 0x44, 0x0f, 800, 0x52, 32);
}

#[test]
fn reply_pressure_and_cancelled_wait_retain_exact_reply_owner() {
    let (mut bearer, mut node) = handoff().split();

    {
        let mut cancelled_receive = pin!(bearer.replies().receive());
        assert!(matches!(
            poll_once(cancelled_receive.as_mut()),
            Poll::Pending
        ));
    }

    node.replies()
        .try_send(reply(2, 20, 0x71, 3))
        .unwrap_or_else(|_| panic!("empty reply channel rejected first owner"));
    let second = node
        .replies()
        .try_send(reply(2, 21, 0x72, 509))
        .expect_err("full reply channel must return second owner")
        .into_inner();
    assert_reply(&second, key(2, 21), 0x72, 509);

    {
        let mut cancelled_capacity = pin!(node.replies().wait_ready_to_send());
        assert!(matches!(
            poll_once(cancelled_capacity.as_mut()),
            Poll::Pending
        ));
    }
    assert_reply(&second, key(2, 21), 0x72, 509);

    let first = bearer
        .replies()
        .try_receive()
        .expect("cancelled reply receive must not consume queued owner");
    assert_reply(&first, key(2, 20), 0x71, 3);

    node.replies()
        .try_send(second)
        .unwrap_or_else(|_| panic!("cancelled capacity wait reserved reply channel"));
    let second = bearer
        .replies()
        .try_receive()
        .expect("retained reply must enqueue unchanged");
    assert_reply(&second, key(2, 21), 0x72, 509);
}

#[test]
fn stale_epoch_reply_is_identifiable_without_touching_completed_mutation() {
    let stale = reply(8, 99, 0x81, 64);
    let current_epoch = SessionEpoch::new(9);
    let reused_correlation = key(9, 99);

    assert!(!stale.belongs_to_epoch(current_epoch));
    assert!(!stale.matches(reused_correlation));
    assert!(stale.matches(key(8, 99)));
    assert_reply(&stale, key(8, 99), 0x81, 64);
}

#[test]
fn lost_response_allows_a_new_idempotent_retry_owner_in_a_new_epoch() {
    let (mut bearer, mut node) = handoff().split();
    let encoded_retry = patterned_buffer(0x91);

    bearer
        .requests()
        .try_send(LocalApiRequest::new(
            key(12, 1),
            grant(0x55, 0x03, 1201),
            OwnedMessage::new(
                MessageLength::new(212).expect("length must fit"),
                encoded_retry,
            ),
        ))
        .unwrap_or_else(|_| panic!("first request must enqueue"));
    let first = node
        .requests()
        .try_receive()
        .expect("node must own first mutation");
    assert_eq!(first.message().full_buffer(), &encoded_retry);

    node.replies()
        .try_send(reply(12, 1, 0x92, 18))
        .unwrap_or_else(|_| panic!("first response must enqueue"));

    let stale = bearer
        .replies()
        .try_receive()
        .expect("old response remains observable after reconnect");
    assert!(!stale.belongs_to_epoch(SessionEpoch::new(13)));
    drop(stale);

    bearer
        .requests()
        .try_send(LocalApiRequest::new(
            key(13, 2),
            grant(0x55, 0x03, 1301),
            OwnedMessage::new(
                MessageLength::new(212).expect("length must fit"),
                encoded_retry,
            ),
        ))
        .unwrap_or_else(|_| panic!("idempotent retry must be a new request owner"));

    let retry = node
        .requests()
        .try_receive()
        .expect("node must independently receive retry owner");
    assert_request(&retry, key(13, 2), 0x55, 0x03, 1301, 0x91, 212);
}

#[test]
fn crossed_correlation_is_visible_even_within_the_same_epoch() {
    let expected = key(21, 210);
    let crossed = reply(21, 211, 0xa1, 7);

    assert!(crossed.belongs_to_epoch(SessionEpoch::new(21)));
    assert!(!crossed.matches(expected));
    assert!(crossed.matches(key(21, 211)));
}

#[test]
fn exact_full_request_and_reply_buffers_survive_async_roundtrip() {
    let (mut bearer, mut node) = handoff().split();
    let request_buffer = patterned_buffer(0xb1);
    let reply_buffer = patterned_buffer(0xc1);

    let request = LocalApiRequest::new(
        key(u64::MAX, u64::MAX - 1),
        grant(0x77, u32::MAX, u64::MAX),
        OwnedMessage::new(
            MessageLength::new(383).expect("length must fit"),
            request_buffer,
        ),
    );
    bearer
        .requests()
        .try_send(request)
        .unwrap_or_else(|_| panic!("request must enqueue"));

    let received = {
        let mut request_wait = pin!(node.requests().receive());
        match poll_once(request_wait.as_mut()) {
            Poll::Ready(request) => request,
            Poll::Pending => panic!("queued request did not wake async receive"),
        }
    };
    let (received_key, received_grant, received_message) = received.into_parts();
    assert_eq!(received_key, key(u64::MAX, u64::MAX - 1));
    assert_eq!(received_grant.principal, [0x77; 16]);
    assert_eq!(received_grant.permissions, u32::MAX);
    assert_eq!(received_grant.mint_nonce, u64::MAX);
    let (received_length, received_buffer) = received_message.into_parts();
    assert_eq!(received_length.get(), 383);
    assert_eq!(received_buffer, request_buffer);

    node.replies()
        .try_send(LocalApiReply::new(
            received_key,
            OwnedMessage::new(
                MessageLength::new(512).expect("length must fit"),
                reply_buffer,
            ),
        ))
        .unwrap_or_else(|_| panic!("reply must enqueue"));
    let mut reply_wait = pin!(bearer.replies().receive());
    let received = match poll_once(reply_wait.as_mut()) {
        Poll::Ready(reply) => reply,
        Poll::Pending => panic!("queued reply did not wake async receive"),
    };
    let (reply_key, reply_message) = received.into_parts();
    let (reply_length, received_reply_buffer) = reply_message.into_parts();
    assert_eq!(reply_key, received_key);
    assert_eq!(reply_length.get(), 512);
    assert_eq!(received_reply_buffer, reply_buffer);
}

#[test]
fn message_length_rejects_oversize_without_taking_a_buffer_owner() {
    let buffer = patterned_buffer(0xd1);
    let error = MessageLength::new(513).expect_err("oversize length must fail");
    assert_eq!(error.length(), 513);
    assert_eq!(buffer, patterned_buffer(0xd1));

    let accepted: Vec<_> = [0, 1, 511, 512]
        .into_iter()
        .map(|length| {
            MessageLength::new(length)
                .expect("bounded length must pass")
                .get()
        })
        .collect();
    assert_eq!(accepted, [0, 1, 511, 512]);
}
