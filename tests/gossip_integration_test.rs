//! Gossip Protocol Integration Tests
//!
//! Tests that verify gossip propagation, deduplication, and TTL behavior
//! using the gossip module directly.

use std::collections::HashSet;
use std::sync::Arc;

use hyperlicked::network::{select_gossip_peers, GossipConfig, GossipMessage, GossipState};
use hyperlicked::types::{Block, ConsensusContext, Message, NodeId, Propose};

/// Create a test node ID from an index
fn test_node_id(n: u8) -> NodeId {
    let mut id = [0u8; 32];
    id[0] = n;
    id
}

/// Create a test propose message
fn test_message(view: u64) -> Message {
    let context = ConsensusContext::new(0, [0u8; 32]);
    Message::Propose(Propose {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        block: Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height: view,
            parent: [0u8; 32],
            payload: vec![],
            proposer: [0u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 0,
            justify: None,
        },
        justify: None,
        proposer_signature: vec![],
    })
}

#[test]
fn test_gossip_deduplication() {
    // Create gossip state with default config
    let config = GossipConfig {
        fanout: 3,
        ttl: 5,
        cache_size: 1000,
        enabled: true,
    };
    let state = GossipState::new(config);

    // Create a message and wrap it
    let msg = test_message(1);
    let origin = test_node_id(1);
    let gossip_msg = state.wrap_message(msg.clone(), origin);

    // First receive should succeed
    let received = state.receive(&gossip_msg);
    assert!(received.is_some(), "First receive should deliver message");

    // Second receive should be deduplicated
    let duplicate = state.receive(&gossip_msg);
    assert!(duplicate.is_none(), "Duplicate should be dropped");

    // Third receive should still be deduplicated
    let duplicate2 = state.receive(&gossip_msg);
    assert!(duplicate2.is_none(), "Duplicate should still be dropped");

    // Stats should show 1 seen message
    let stats = state.stats();
    assert_eq!(stats.seen_count, 1, "Should have 1 seen message");
}

#[test]
fn test_gossip_ttl_limits_propagation() {
    let config = GossipConfig {
        fanout: 1,
        ttl: 3, // Messages can travel 3 hops
        cache_size: 1000,
        enabled: true,
    };
    let _state = GossipState::new(config);

    // Create initial message with TTL=3
    let msg = test_message(1);
    let origin = test_node_id(1);
    let gossip_msg = GossipMessage::new(msg, 3, origin);

    // TTL=3: should relay
    assert!(gossip_msg.should_relay(), "TTL=3 should relay");
    let hop1 = gossip_msg.relay().expect("Should relay");
    assert_eq!(hop1.ttl, 2);

    // TTL=2: should relay
    assert!(hop1.should_relay(), "TTL=2 should relay");
    let hop2 = hop1.relay().expect("Should relay");
    assert_eq!(hop2.ttl, 1);

    // TTL=1: should NOT relay (last hop)
    assert!(!hop2.should_relay(), "TTL=1 should NOT relay");
    let hop3 = hop2.relay();
    assert!(hop3.is_none(), "TTL=1 relay should return None");
}

#[test]
fn test_gossip_peer_selection() {
    // Create 10 peers
    let peers: Vec<NodeId> = (1..=10).map(test_node_id).collect();
    let msg_id = [42u8; 32];

    // Test fanout=3
    let selected = select_gossip_peers(&peers, &msg_id, 3, None);
    assert_eq!(selected.len(), 3, "Should select 3 peers");

    // All selected should be unique
    let unique: HashSet<_> = selected.iter().collect();
    assert_eq!(unique.len(), 3, "Selected peers should be unique");

    // Same msg_id should select same peers (deterministic)
    let selected2 = select_gossip_peers(&peers, &msg_id, 3, None);
    assert_eq!(selected, selected2, "Selection should be deterministic");

    // Different msg_id should likely select different peers
    let different_id = [99u8; 32];
    let selected3 = select_gossip_peers(&peers, &different_id, 3, None);
    // Not guaranteed to be different but very likely
    // We just check it returns the right count
    assert_eq!(selected3.len(), 3);
}

#[test]
fn test_gossip_peer_selection_with_exclusion() {
    let peers: Vec<NodeId> = (1..=5).map(test_node_id).collect();
    let msg_id = [42u8; 32];
    let exclude = test_node_id(3);

    let selected = select_gossip_peers(&peers, &msg_id, 5, Some(&exclude));

    // Should not include excluded peer
    assert!(!selected.contains(&exclude), "Should exclude sender");
    assert_eq!(selected.len(), 4, "Should select all except excluded");
}

#[test]
fn test_gossip_disabled_fallback() {
    let config = GossipConfig {
        fanout: 3,
        ttl: 5,
        cache_size: 1000,
        enabled: false, // Disabled
    };
    let gossip_state = GossipState::new(config);

    assert!(!gossip_state.is_enabled(), "Gossip should be disabled");
    assert_eq!(gossip_state.fanout(), 3);
    assert_eq!(gossip_state.initial_ttl(), 5);
}

#[test]
fn test_gossip_seen_cache_eviction() {
    // Small cache to test eviction
    let config = GossipConfig {
        fanout: 3,
        ttl: 5,
        cache_size: 5,
        enabled: true,
    };
    let state = GossipState::new(config);

    // Insert 5 messages
    for i in 0..5 {
        let msg = test_message(i);
        let gossip_msg = state.wrap_message(msg, test_node_id(1));
        state.mark_seen(&gossip_msg.msg_id);
    }

    let stats = state.stats();
    assert_eq!(stats.seen_count, 5, "Should have 5 seen messages");

    // Insert one more, should evict oldest
    let msg = test_message(100);
    let gossip_msg = state.wrap_message(msg, test_node_id(1));
    state.mark_seen(&gossip_msg.msg_id);

    let stats = state.stats();
    assert_eq!(stats.seen_count, 5, "Should still have 5 (evicted oldest)");
}

#[test]
fn test_gossip_message_id_deterministic() {
    let msg = test_message(1);
    let origin = test_node_id(1);

    // Same message should produce same ID
    let gm1 = GossipMessage::new(msg.clone(), 5, origin);
    let gm2 = GossipMessage::new(msg.clone(), 5, origin);

    assert_eq!(gm1.msg_id, gm2.msg_id, "Same message should have same ID");

    // Different TTL shouldn't affect message ID (ID is content-based)
    let gm3 = GossipMessage::new(msg.clone(), 3, origin);
    assert_eq!(gm1.msg_id, gm3.msg_id, "TTL shouldn't affect message ID");

    // Different origin shouldn't affect message ID (ID is content-based)
    let gm4 = GossipMessage::new(msg, 5, test_node_id(2));
    assert_eq!(gm1.msg_id, gm4.msg_id, "Origin shouldn't affect message ID");
}

#[test]
fn test_gossip_config_from_env() {
    // Default values when env vars not set
    // Note: This test may fail if env vars are set globally
    let config = GossipConfig::default();
    assert_eq!(config.fanout, 3);
    assert_eq!(config.ttl, 5);
    assert_eq!(config.cache_size, 10_000);
    assert!(config.enabled);
}

/// Simulate multi-node gossip propagation
#[test]
fn test_gossip_multi_node_simulation() {
    // Simulate 5 nodes with gossip state
    let config = GossipConfig {
        fanout: 2,
        ttl: 3,
        cache_size: 1000,
        enabled: true,
    };

    let states: Vec<Arc<GossipState>> = (0..5)
        .map(|_| Arc::new(GossipState::new(config.clone())))
        .collect();

    let node_ids: Vec<NodeId> = (1..=5).map(test_node_id).collect();

    // Node 0 creates and broadcasts a message
    let msg = test_message(1);
    let origin = node_ids[0];
    let gossip_msg = states[0].wrap_message(msg, origin);

    // Node 0 marks as seen
    states[0].mark_seen(&gossip_msg.msg_id);

    // Simulate delivery to nodes 1 and 2 (fanout=2)
    let delivered_to_1 = states[1].receive(&gossip_msg).is_some();
    let delivered_to_2 = states[2].receive(&gossip_msg).is_some();

    assert!(delivered_to_1, "Node 1 should receive message");
    assert!(delivered_to_2, "Node 2 should receive message");

    // Node 1 relays to nodes 3 and 4
    if let Some(relayed) = gossip_msg.relay() {
        let delivered_to_3 = states[3].receive(&relayed).is_some();
        let delivered_to_4 = states[4].receive(&relayed).is_some();

        assert!(delivered_to_3, "Node 3 should receive relayed message");
        assert!(delivered_to_4, "Node 4 should receive relayed message");
    }

    // If node 2 also tries to relay to node 3, it should be deduplicated
    if let Some(relayed) = gossip_msg.relay() {
        let duplicate_to_3 = states[3].receive(&relayed).is_some();
        assert!(!duplicate_to_3, "Node 3 should deduplicate");
    }

    // All nodes should have seen exactly 1 message
    for (i, state) in states.iter().enumerate() {
        // Node 0 marked seen, nodes 1-4 received
        let expected = 1;
        assert_eq!(
            state.stats().seen_count,
            expected,
            "Node {} should have {} seen messages",
            i,
            expected
        );
    }
}

/// Test that gossip wrapping preserves message content
#[test]
fn test_gossip_message_content_preserved() {
    let config = GossipConfig::default();
    let state = GossipState::new(config);

    let original = test_message(42);
    let origin = test_node_id(1);
    let gossip_msg = state.wrap_message(original.clone(), origin);

    // Receive the message
    let received = state.receive(&gossip_msg).expect("Should deliver");

    // Compare message types
    match (&original, &received) {
        (Message::Propose(orig), Message::Propose(recv)) => {
            assert_eq!(orig.block.view, recv.block.view);
            assert_eq!(orig.block.height, recv.block.height);
        }
        _ => panic!("Message type mismatch"),
    }
}

/// Test edge cases for peer selection
#[test]
fn test_gossip_peer_selection_edge_cases() {
    let msg_id = [42u8; 32];

    // Empty peers
    let empty: Vec<NodeId> = vec![];
    let selected = select_gossip_peers(&empty, &msg_id, 3, None);
    assert!(selected.is_empty(), "Empty peers should return empty");

    // Fanout = 0
    let peers: Vec<NodeId> = (1..=5).map(test_node_id).collect();
    let selected = select_gossip_peers(&peers, &msg_id, 0, None);
    assert!(selected.is_empty(), "Fanout 0 should return empty");

    // Fanout > peers
    let selected = select_gossip_peers(&peers, &msg_id, 10, None);
    assert_eq!(selected.len(), 5, "Should select all available peers");

    // Only one peer and it's excluded
    let single = vec![test_node_id(1)];
    let selected = select_gossip_peers(&single, &msg_id, 1, Some(&test_node_id(1)));
    assert!(
        selected.is_empty(),
        "Excluding only peer should return empty"
    );
}
