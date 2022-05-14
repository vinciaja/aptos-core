// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::smoke_test_environment::new_local_swarm_with_aptos;
use aptos::op::key::GenerateKey;
use aptos_config::{
    config::{DiscoveryMethod, Identity, NetworkConfig, NodeConfig, PeerSet, PersistableConfig},
    network_id::NetworkId,
};
use aptos_crypto::{x25519, x25519::PrivateKey};
use aptos_operational_tool::{keys::EncodingType, test_helper::OperationalTool};
use aptos_temppath::TempPath;
use aptos_types::network_address::{NetworkAddress, Protocol};
use forge::{FullNode, LocalNode, NodeExt, Swarm};
use std::{
    collections::HashMap,
    path::Path,
    str::FromStr,
    time::{Duration, Instant},
};

#[tokio::test]
async fn test_connection_limiting() {
    let mut swarm = new_local_swarm_with_aptos(1).await;
    let version = swarm.versions().max().unwrap();
    let validator_peer_id = swarm.validators().next().unwrap().peer_id();
    let vfn_peer_id = swarm
        .add_validator_fullnode(
            &version,
            NodeConfig::default_for_validator_full_node(),
            validator_peer_id,
        )
        .await
        .unwrap();

    let op_tool = OperationalTool::test();
    let (private_key, peer_set) = generate_private_key_and_peer(&op_tool).await;
    let discovery_file = create_discovery_file(peer_set.clone());

    // Only allow file based discovery, disallow other nodes
    modify_network_of_node(
        swarm.fullnode_mut(vfn_peer_id).unwrap(),
        &NetworkId::Public,
        |network| {
            network.discovery_method = DiscoveryMethod::None;
            network.discovery_methods = vec![
                DiscoveryMethod::Onchain,
                DiscoveryMethod::File(
                    discovery_file.as_ref().to_path_buf(),
                    Duration::from_secs(1),
                ),
            ];
            network.max_inbound_connections = 0;
        },
    )
    .await;

    // Wait till nodes are healthy
    swarm
        .validator_mut(validator_peer_id)
        .unwrap()
        .wait_until_healthy(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    swarm
        .fullnode_mut(vfn_peer_id)
        .unwrap()
        .wait_until_healthy(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();

    // This node should be able to connect
    let pfn_peer_id = swarm
        .add_full_node(&version, NodeConfig::default_for_public_full_node())
        .unwrap();
    add_identity_to_node(
        swarm.fullnode_mut(pfn_peer_id).unwrap(),
        &NetworkId::Public,
        private_key,
        peer_set,
    )
    .await;
    swarm
        .fullnode_mut(pfn_peer_id)
        .unwrap()
        .wait_until_healthy(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    // This node should connect
    FullNode::wait_for_connectivity(
        swarm.fullnode(pfn_peer_id).unwrap(),
        Instant::now() + Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(
        1,
        swarm
            .fullnode(vfn_peer_id)
            .unwrap()
            .get_connected_peers(NetworkId::Public, Some("inbound"))
            .await
            .unwrap()
            .unwrap_or(0)
    );

    // And not be able to connect with an arbitrary one, limit is 0
    // TODO: Improve network checker to keep connection alive so we can test connection limits without nodes
    let (private_key, peer_set) = generate_private_key_and_peer(&op_tool).await;
    let pfn_peer_id_fail = swarm
        .add_full_node(&version, NodeConfig::default_for_public_full_node())
        .unwrap();
    add_identity_to_node(
        swarm.fullnode_mut(pfn_peer_id_fail).unwrap(),
        &NetworkId::Public,
        private_key,
        peer_set,
    )
    .await;

    // This node should fail to connect
    swarm
        .fullnode_mut(pfn_peer_id_fail)
        .unwrap()
        .wait_until_healthy(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        1,
        swarm
            .fullnode(vfn_peer_id)
            .unwrap()
            .get_connected_peers(NetworkId::Public, Some("inbound"))
            .await
            .unwrap()
            .unwrap_or(0)
    );
}

// Currently this test seems flaky: https://github.com/aptos-labs/aptos-core/issues/670
#[ignore]
#[tokio::test]
async fn test_file_discovery() {
    let mut swarm = new_local_swarm_with_aptos(1).await;
    let validator_peer_id = swarm.validators().next().unwrap().peer_id();
    let op_tool = OperationalTool::test();
    let (private_key, peer_set) = generate_private_key_and_peer(&op_tool).await;
    let discovery_file = create_discovery_file(peer_set);

    // Add key to file based discovery
    modify_network_of_node(
        swarm.validator_mut(validator_peer_id).unwrap(),
        &NetworkId::Validator,
        |network| {
            network.discovery_method = DiscoveryMethod::None;
            network.discovery_methods = vec![
                DiscoveryMethod::Onchain,
                DiscoveryMethod::File(
                    discovery_file.as_ref().to_path_buf(),
                    Duration::from_millis(100),
                ),
            ];
        },
    )
    .await;

    // Startup the validator
    swarm.launch().await.unwrap();

    // At first we should be able to connect
    assert_eq!(
        true,
        check_endpoint(
            &op_tool,
            NetworkId::Validator,
            swarm.validator(validator_peer_id).unwrap(),
            &private_key
        )
        .await
    );

    // Now when we clear the file, we shouldn't be able to connect
    write_peerset_to_file(discovery_file.as_ref(), HashMap::new());
    std::thread::sleep(Duration::from_millis(300));

    assert_eq!(
        false,
        check_endpoint(
            &op_tool,
            NetworkId::Validator,
            swarm.validator(validator_peer_id).unwrap(),
            &private_key
        )
        .await
    );
}

/// Creates a discovery file with the given `PeerSet`
fn create_discovery_file(peer_set: PeerSet) -> TempPath {
    let discovery_file = TempPath::new();
    discovery_file.create_as_file().unwrap();
    write_peerset_to_file(discovery_file.as_ref(), peer_set);
    discovery_file
}

/// Generates `PrivateKey` and `Peer` information for a client / node
async fn generate_private_key_and_peer(op_tool: &OperationalTool) -> (PrivateKey, PeerSet) {
    let key_file = TempPath::new();
    key_file.create_as_file().unwrap();
    let (private_key, _) =
        GenerateKey::generate_x25519(aptos::common::types::EncodingType::BCS, key_file.as_ref())
            .await
            .unwrap();
    let peer_set = op_tool
        .extract_peer_from_file(key_file.as_ref(), EncodingType::BCS)
        .await
        .unwrap();
    (private_key, peer_set)
}

/// Modifies a network on the on disk configuration.  Needs to be done prior to starting node
async fn modify_network_of_node<F: FnOnce(&mut NetworkConfig)>(
    node: &mut LocalNode,
    network_id: &NetworkId,
    modifier: F,
) {
    let node_config_path = node.config_path();
    let mut node_config = NodeConfig::load(&node_config_path).unwrap();
    modify_network_config(&mut node_config, network_id, modifier);
    node_config.save_config(node_config_path).unwrap();
    node.restart().await.unwrap();
}

fn modify_network_config<F: FnOnce(&mut NetworkConfig)>(
    node_config: &mut NodeConfig,
    network_id: &NetworkId,
    modifier: F,
) {
    let network = match network_id {
        NetworkId::Validator => node_config.validator_network.as_mut().unwrap(),
        _ => node_config
            .full_node_networks
            .iter_mut()
            .find(|network| &network.network_id == network_id)
            .unwrap(),
    };

    modifier(network)
}

async fn add_identity_to_node(
    node: &mut LocalNode,
    network_id: &NetworkId,
    private_key: PrivateKey,
    peer_set: PeerSet,
) {
    let (peer_id, _) = peer_set.iter().next().unwrap();
    modify_network_of_node(node, network_id, |network| {
        network.identity = Identity::from_config(private_key, *peer_id);
    })
    .await;
}

async fn check_endpoint(
    op_tool: &OperationalTool,
    network_id: NetworkId,
    node: &LocalNode,
    private_key: &x25519::PrivateKey,
) -> bool {
    let address = network_address(node.config(), &network_id);
    let result = op_tool
        .check_endpoint_with_key(&network_id, address.clone(), private_key)
        .await;
    println!(
        "Endpoint check for {}:{} is:  {:?}",
        network_id, address, result
    );
    result.is_ok()
}

fn network_address(node_config: &NodeConfig, network_id: &NetworkId) -> NetworkAddress {
    let network = network(node_config, network_id);

    let port = network
        .listen_address
        .as_slice()
        .iter()
        .find_map(|proto| {
            if let Protocol::Tcp(port) = proto {
                Some(port)
            } else {
                None
            }
        })
        .unwrap();
    let key = network.identity_key().public_key();
    NetworkAddress::from_str(&format!(
        "/ip4/127.0.0.1/tcp/{}/ln-noise-ik/{}/ln-handshake/0",
        port, key
    ))
    .unwrap()
}

fn network<'a>(node_config: &'a NodeConfig, network_id: &NetworkId) -> &'a NetworkConfig {
    match network_id {
        NetworkId::Validator => node_config.validator_network.as_ref().unwrap(),
        _ => node_config
            .full_node_networks
            .iter()
            .find(|network| network.network_id == *network_id)
            .unwrap(),
    }
}

pub fn write_peerset_to_file(path: &Path, peers: PeerSet) {
    let file_contents = serde_yaml::to_vec(&peers).unwrap();
    std::fs::write(path, file_contents).unwrap();
}
