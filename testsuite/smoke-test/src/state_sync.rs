// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{
    smoke_test_environment::new_local_swarm_with_aptos,
    test_utils::{assert_balance, create_and_fund_account, transfer_coins},
};
use forge::{NodeExt, Swarm, SwarmExt};
use std::{
    fs,
    time::{Duration, Instant},
};

#[tokio::test]
async fn test_basic_state_synchronization() {
    // - Start a swarm of 4 nodes (3 nodes forming a QC).
    // - Kill one node and continue submitting transactions to the others.
    // - Restart the node
    // - Wait for all the nodes to catch up
    // - Verify that the restarted node has synced up with the submitted transactions.

    // we set a smaller chunk limit (=5) here to properly test multi-chunk state sync
    let mut swarm = new_local_swarm_with_aptos(4).await;
    for validator in swarm.validators_mut() {
        let mut config = validator.config().clone();
        config.state_sync.chunk_limit = 5;
        config.save(validator.config_path()).unwrap();
        validator.restart().await.unwrap();
    }
    swarm.launch().await.unwrap(); // Make sure all nodes are healthy and live
    let validator_peer_ids = swarm.validators().map(|v| v.peer_id()).collect::<Vec<_>>();

    let client_1 = swarm
        .validator(validator_peer_ids[1])
        .unwrap()
        .rest_client();
    let transaction_factory = swarm.chain_info().transaction_factory();

    let mut account_0 = create_and_fund_account(&mut swarm, 100).await;
    let account_1 = create_and_fund_account(&mut swarm, 10).await;

    transfer_coins(
        &client_1,
        &transaction_factory,
        &mut account_0,
        &account_1,
        10,
    )
    .await;
    assert_balance(&client_1, &account_0, 90).await;
    assert_balance(&client_1, &account_1, 20).await;

    // Stop a node
    let node_to_restart = validator_peer_ids[0];
    swarm.validator_mut(node_to_restart).unwrap().stop();

    // Do a transfer and ensure it still executes
    transfer_coins(
        &client_1,
        &transaction_factory,
        &mut account_0,
        &account_1,
        1,
    )
    .await;
    assert_balance(&client_1, &account_0, 89).await;
    assert_balance(&client_1, &account_1, 21).await;

    // Restart killed node and wait for all nodes to catchup
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .start()
        .unwrap();
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .wait_until_healthy(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    swarm
        .wait_for_all_nodes_to_catchup(Instant::now() + Duration::from_secs(60))
        .await
        .unwrap();

    // Connect to the newly recovered node and verify its state
    let client_0 = swarm.validator(node_to_restart).unwrap().rest_client();
    assert_balance(&client_0, &account_0, 89).await;
    assert_balance(&client_0, &account_1, 21).await;

    // Test multiple chunk sync
    swarm.validator_mut(node_to_restart).unwrap().stop();

    for _ in 0..10 {
        transfer_coins(
            &client_1,
            &transaction_factory,
            &mut account_0,
            &account_1,
            1,
        )
        .await;
    }

    assert_balance(&client_1, &account_0, 79).await;
    assert_balance(&client_1, &account_1, 31).await;

    // Restart killed node and wait for all nodes to catchup
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .start()
        .unwrap();
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .wait_until_healthy(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    swarm
        .wait_for_all_nodes_to_catchup(Instant::now() + Duration::from_secs(60))
        .await
        .unwrap();

    assert_balance(&client_0, &account_0, 79).await;
    assert_balance(&client_0, &account_1, 31).await;
}

#[tokio::test]
async fn test_startup_sync_state() {
    let mut swarm = new_local_swarm_with_aptos(4).await;
    let validator_peer_ids = swarm.validators().map(|v| v.peer_id()).collect::<Vec<_>>();
    let client_1 = swarm
        .validator(validator_peer_ids[1])
        .unwrap()
        .rest_client();
    let transaction_factory = swarm.chain_info().transaction_factory();

    let mut account_0 = create_and_fund_account(&mut swarm, 100).await;
    let account_1 = create_and_fund_account(&mut swarm, 10).await;

    let txn = transfer_coins(
        &client_1,
        &transaction_factory,
        &mut account_0,
        &account_1,
        10,
    )
    .await;
    assert_balance(&client_1, &account_0, 90).await;
    assert_balance(&client_1, &account_1, 20).await;

    // Stop a node
    let node_to_restart = validator_peer_ids[0];
    let node_config = swarm.validator(node_to_restart).unwrap().config().clone();
    swarm.validator_mut(node_to_restart).unwrap().stop();
    // TODO Remove hardcoded path to state db
    let state_db_path = node_config.storage.dir().join("aptosdb");
    // Verify that state_db_path exists and
    // we are not deleting a non-existent directory
    assert!(state_db_path.as_path().exists());
    // Delete the state db to simulate state db lagging
    // behind consensus db and forcing a state sync
    // during a node startup
    fs::remove_dir_all(state_db_path).unwrap();
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .start()
        .unwrap();
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .wait_until_healthy(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();

    let client_0 = swarm.validator(node_to_restart).unwrap().rest_client();
    // Wait for the txn to by synced to the restarted node
    client_0.wait_for_signed_transaction(&txn).await.unwrap();
    assert_balance(&client_0, &account_0, 90).await;
    assert_balance(&client_0, &account_1, 20).await;

    let txn = transfer_coins(
        &client_1,
        &transaction_factory,
        &mut account_0,
        &account_1,
        10,
    )
    .await;
    client_0.wait_for_signed_transaction(&txn).await.unwrap();

    assert_balance(&client_0, &account_0, 80).await;
    assert_balance(&client_0, &account_1, 30).await;
}

/*
 * Diabled until we can add waypoints to rest interface
#[tokio::test]
async fn test_state_sync_multichunk_epoch() {
    let mut swarm = new_local_swarm(4).await;
    for validator in swarm.validators_mut() {
        let mut config = validator.config().clone();
        config.state_sync.chunk_limit = 5;
        config.save(validator.config_path()).unwrap();
        validator.restart().await.unwrap();
    }
    swarm.launch().await.unwrap(); // Make sure all nodes are healthy and live
    let validator_peer_ids = swarm.validators().map(|v| v.peer_id()).collect::<Vec<_>>();

    let client_0 = swarm
        .validator(validator_peer_ids[0])
        .unwrap()
        .rest_client();
    let transaction_factory = swarm.chain_info().transaction_factory();

    enable_open_publishing(
        &client_0,
        &transaction_factory,
        swarm.chain_info().root_account,
    )
    .await
    .unwrap();

    let mut account_0 = create_and_fund_account(&mut swarm, 100).await;
    let account_1 = create_and_fund_account(&mut swarm, 10).await;
    assert_balance(&client_0, &account_0, 100).await;
    assert_balance(&client_0, &account_1, 10).await;

    // we bring this validator back up with waypoint s.t. the waypoint sync spans multiple epochs,
    // and each epoch spanning multiple chunks
    let node_to_restart = validator_peer_ids[3];
    swarm.validator_mut(node_to_restart).unwrap().stop();

    // submit more transactions to make the current epoch (=1) span > 1 chunk (= 5 versions)
    for _ in 0..7 {
        transfer_coins(
            &client_0,
            &transaction_factory,
            &mut account_0,
            &account_1,
            10,
        )
        .await;
    }

    // Bump epoch by trigger a reconfig for multiple epochs
    for curr_epoch in 2u64..=3 {
        // bumps epoch from curr_epoch -> curr_epoch + 1
        enable_open_publishing(
            &client_0,
            &transaction_factory,
            swarm.chain_info().root_account,
        )
        .await
        .unwrap();

        let next_block_epoch = *client_0
            .get_epoch_configuration()
            .await
            .unwrap()
            .into_inner()
            .next_block_epoch
            .inner();
        assert_eq!(next_block_epoch, curr_epoch + 1);
    }

    let json_rpc_client_0 = swarm
        .validator(validator_peer_ids[0])
        .unwrap()
        .async_json_rpc_client();
    // bring back dead validator with waypoint
    let epoch_change_proof: EpochChangeProof = bcs::from_bytes(
        json_rpc_client_0
            .get_state_proof(0)
            .await
            .unwrap()
            .into_inner()
            .epoch_change_proof
            .inner(),
    )
    .unwrap();
    let waypoint_epoch_2 = Waypoint::new_epoch_boundary(
        epoch_change_proof
            .ledger_info_with_sigs
            .last()
            .unwrap()
            .ledger_info(),
    )
    .unwrap();

    let node_config_path = swarm.validator(node_to_restart).unwrap().config_path();
    let mut node_config = swarm.validator(node_to_restart).unwrap().config().clone();
    node_config.execution.genesis = None;
    node_config.execution.genesis_file_location = PathBuf::from("");
    insert_waypoint(&mut node_config, waypoint_epoch_2);
    node_config.save(node_config_path).unwrap();

    // Restart killed node and wait for all nodes to catchup
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .start()
        .unwrap();
    swarm
        .validator_mut(node_to_restart)
        .unwrap()
        .wait_until_healthy(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    swarm
        .wait_for_all_nodes_to_catchup(Instant::now() + Duration::from_secs(60))
        .await
        .unwrap();
}
*/
