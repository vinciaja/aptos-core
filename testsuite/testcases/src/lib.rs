// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

pub mod compatibility_test;
pub mod fixed_tps_test;
pub mod gas_price_test;
pub mod partial_nodes_down_test;
pub mod performance_test;
pub mod reconfiguration_test;
pub mod state_sync_performance;

use anyhow::ensure;
use aptos_sdk::{transaction_builder::TransactionFactory, types::PeerId};
use forge::{NetworkContext, NodeExt, Result, TxnEmitter, TxnStats, Version};
use rand::SeedableRng;
use std::{
    convert::TryInto,
    time::{Duration, Instant},
};
use tokio::runtime::Runtime;

async fn batch_update(
    ctx: &mut NetworkContext<'_>,
    validators_to_update: &[PeerId],
    version: &Version,
) -> Result<()> {
    for validator in validators_to_update {
        ctx.swarm().upgrade_validator(*validator, version)?;
    }

    ctx.swarm().health_check().await?;
    let deadline = Instant::now() + Duration::from_secs(60);
    for validator in validators_to_update {
        ctx.swarm()
            .validator_mut(*validator)
            .unwrap()
            .wait_until_healthy(deadline)
            .await?;
    }

    Ok(())
}

pub fn generate_traffic<'t>(
    ctx: &mut NetworkContext<'t>,
    validators: &[PeerId],
    duration: Duration,
    gas_price: u64,
    fixed_tps: Option<u64>,
) -> Result<TxnStats> {
    ensure!(gas_price > 0, "gas_price is required to be non zero");
    let rt = Runtime::new()?;
    let rng = SeedableRng::from_rng(ctx.core().rng())?;
    let validator_clients = ctx
        .swarm()
        .validators()
        .filter(|v| validators.contains(&v.peer_id()))
        .map(|n| n.rest_client())
        .collect::<Vec<_>>();
    let mut emit_job_request = ctx.global_job.clone();
    let chain_info = ctx.swarm().chain_info();
    let transaction_factory = TransactionFactory::new(chain_info.chain_id).with_gas_unit_price(1);
    let mut emitter = TxnEmitter::new(
        chain_info.root_account,
        validator_clients[0].clone(),
        transaction_factory,
        rng,
    );

    emit_job_request = emit_job_request
        .rest_clients(validator_clients)
        .gas_price(gas_price);
    if let Some(target_tps) = fixed_tps {
        emit_job_request = emit_job_request.fixed_tps(target_tps.try_into().unwrap());
    }
    let stats = rt.block_on(emitter.emit_txn_for(duration, emit_job_request))?;

    Ok(stats)
}
