//! T14: lease ownership moves between two nodes through the cluster transport.
//!
//! These are the offline (in-process) twins of the NATS two-node test: two
//! independent `RoomOwner` stores, each owned by its own node, exchange lease
//! events as wire-encoded [`vks_cluster::Msg`]s over a `Transport`. The same
//! test body is designed to run unchanged over `NatsTransport` when the
//! `nats` feature lands (see `docs/operations/cluster-bus.md`); today the
//! transport under test is the documented `LocalTransport` fallback.

use std::sync::Arc;

use vks_cluster::{
    decode_msg, encode_msg, select_transport, LeaseResult, LocalTransport, Msg, RoomOwner, Transport, TransportKind,
};

const ROOM: &str = "room-lease-move";
const LEASE_UPDATE_TOPIC: &str = "vks.cluster.lease.update";
const LEASE_RELEASE_TOPIC: &str = "vks.cluster.lease.release";

/// Publishes the lease event for `room`; payload is the owner id (the test
/// deliberately exercises the versioned envelope, not a domain JSON schema).
fn publish_lease(transport: &dyn Transport, topic: &str, owner: &str, epoch: u64) {
    let payload = format!("{owner}|{epoch}").into_bytes();
    transport.publish(Msg::new(topic, payload, true)).expect("publish lease event");
}

/// Reads one event, proving it survives the stable wire format.
fn receive_lease(transport: &dyn Transport, topic: &str) -> (String, u64) {
    let msgs = transport.read(topic);
    assert_eq!(msgs.len(), 1, "expected exactly one lease event on {topic}");
    let decoded = decode_msg(&encode_msg(&msgs[0])).expect("wire roundtrip");
    assert_eq!(decoded.id, msgs[0].id, "wire envelope must preserve UUIDv7 id");
    let payload = std::str::from_utf8(&decoded.payload).expect("owner payload is UTF-8");
    let (owner, epoch) = payload.split_once('|').expect("owner|epoch");
    (owner.to_string(), epoch.parse().expect("epoch"))
}

#[test]
fn node_b_acquires_after_node_a_leaves_voluntarily() {
    let transport: Arc<dyn Transport> = Arc::new(LocalTransport::new());
    let mut node_a = RoomOwner::new();
    let mut node_b = RoomOwner::new();

    // 1. Node A wins the free lease (epoch 1) and announces it.
    assert_eq!(node_a.acquire_lease(ROOM, "node-a", 0, 30), LeaseResult::Acquired);
    publish_lease(transport.as_ref(), LEASE_UPDATE_TOPIC, "node-a", node_a.epoch(ROOM).expect("epoch"));

    // 2. Node B receives the event and mirrors the owner in its own store.
    let (owner, epoch) = receive_lease(transport.as_ref(), LEASE_UPDATE_TOPIC);
    assert_eq!((owner.as_str(), epoch), ("node-a", 1));
    assert_eq!(node_b.acquire_lease(ROOM, &owner, 0, 30), LeaseResult::Acquired);
    assert_eq!(node_b.owner(ROOM), Some("node-a"));

    // 3. A leaves voluntarily: release locally + publish the release event.
    assert!(node_a.release(ROOM, "node-a"), "owner must release its own lease");
    publish_lease(transport.as_ref(), LEASE_RELEASE_TOPIC, "node-a", 1);
    let (released_by, _) = receive_lease(transport.as_ref(), LEASE_RELEASE_TOPIC);
    assert!(node_b.release(ROOM, &released_by), "release event must free the mirrored lease");
    assert_eq!(node_b.owner(ROOM), None);

    // 4. B acquires the now-free room; the fencing epoch never goes backwards.
    assert_eq!(node_b.acquire_lease(ROOM, "node-b", 5, 30), LeaseResult::Acquired);
    assert_eq!(node_b.owner(ROOM), Some("node-b"));
    assert_eq!(node_b.epoch(ROOM), Some(2));
    assert_eq!(node_a.epoch(ROOM), None, "A dropped its lease on release");
}

#[test]
fn node_b_acquires_after_node_a_dies_without_release() {
    let transport: Arc<dyn Transport> = Arc::new(LocalTransport::new());
    let mut node_a = RoomOwner::new();
    let mut node_b = RoomOwner::new();

    // A holds the lease until tick 30 and then vanishes (no release event).
    assert_eq!(node_a.acquire_lease(ROOM, "node-a", 0, 30), LeaseResult::Acquired);
    publish_lease(transport.as_ref(), LEASE_UPDATE_TOPIC, "node-a", 1);
    let (owner, _) = receive_lease(transport.as_ref(), LEASE_UPDATE_TOPIC);
    assert_eq!(node_b.acquire_lease(ROOM, &owner, 0, 30), LeaseResult::Acquired);
    assert_eq!(node_b.active_owner(ROOM, 29), Some("node-a"));

    // At tick 100 the mirrored lease is long expired: a dead owner must not
    // block failover, and the epoch advances so stale fenced writes are rejected.
    assert_eq!(node_b.active_owner(ROOM, 100), None);
    assert_eq!(node_b.acquire_lease(ROOM, "node-b", 100, 30), LeaseResult::Acquired);
    assert_eq!(node_b.owner(ROOM), Some("node-b"));
    assert_eq!(node_b.epoch(ROOM), Some(2));
    assert_eq!(node_b.lease_expiry(ROOM), Some(130));
}

#[test]
fn local_transport_is_the_documented_fallback_when_nats_is_unset() {
    let selection = select_transport(None);
    assert_eq!(selection.transport.kind(), TransportKind::Local);
    assert!(selection.fallback_reason.is_none(), "unset NATS is a normal single-node mode, not a degraded one");
    assert!(!selection.transport.is_remote());
}

#[test]
fn wire_message_is_stable_across_two_transports() {
    // Both nodes exchange the *bytes*, so a serialization drift must surface
    // here even before a real NATS broker exists.
    let node_a_bus = Arc::new(LocalTransport::new());
    let node_b_bus = Arc::new(LocalTransport::new());
    node_a_bus.bus().subscribe(LEASE_UPDATE_TOPIC);

    let msg = Msg::new(LEASE_UPDATE_TOPIC, b"node-a|1".to_vec(), true);
    let bytes = encode_msg(&msg);
    let decoded = decode_msg(&bytes).expect("decode");
    node_b_bus.publish(decoded).expect("publish to B");
    assert_eq!(node_b_bus.read(LEASE_UPDATE_TOPIC).len(), 1);
    assert_eq!(encode_msg(&node_b_bus.read(LEASE_UPDATE_TOPIC)[0]), bytes);
}
