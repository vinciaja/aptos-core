// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{block::Block, vote_data::VoteData};
use aptos_crypto::{
    ed25519::Ed25519Signature,
    hash::{TransactionAccumulatorHasher, ACCUMULATOR_PLACEHOLDER_HASH},
};
use aptos_crypto_derive::{BCSCryptoHash, CryptoHasher};
use aptos_types::{
    epoch_state::EpochState,
    proof::{accumulator::InMemoryAccumulator, AccumulatorExtensionProof},
};
use serde::{Deserialize, Serialize};
use std::{
    fmt::{Display, Formatter},
    ops::Deref,
};

/// This structure contains all the information needed by safety rules to
/// evaluate a proposal / block for correctness / safety and to produce a Vote.
#[derive(Clone, Debug, CryptoHasher, Deserialize, BCSCryptoHash, Serialize)]
pub struct VoteProposal {
    /// Contains the data necessary to construct the parent's execution output state
    /// and the childs in a verifiable way
    accumulator_extension_proof: AccumulatorExtensionProof<TransactionAccumulatorHasher>,
    /// The block / proposal to evaluate
    #[serde(bound(deserialize = "Block: Deserialize<'de>"))]
    block: Block,
    /// An optional field containing the next epoch info.
    next_epoch_state: Option<EpochState>,
    /// Represents whether the executed state id is dummy or not.
    decoupled_execution: bool,
}

impl VoteProposal {
    pub fn new(
        accumulator_extension_proof: AccumulatorExtensionProof<TransactionAccumulatorHasher>,
        block: Block,
        next_epoch_state: Option<EpochState>,
        decoupled_execution: bool,
    ) -> Self {
        Self {
            accumulator_extension_proof,
            block,
            next_epoch_state,
            decoupled_execution,
        }
    }

    pub fn accumulator_extension_proof(
        &self,
    ) -> &AccumulatorExtensionProof<TransactionAccumulatorHasher> {
        &self.accumulator_extension_proof
    }

    pub fn block(&self) -> &Block {
        &self.block
    }

    pub fn next_epoch_state(&self) -> Option<&EpochState> {
        self.next_epoch_state.as_ref()
    }

    /// This function returns the vote data with a dummy executed_state_id and version
    fn vote_data_ordering_only(&self) -> VoteData {
        VoteData::new(
            self.block().gen_block_info(
                *ACCUMULATOR_PLACEHOLDER_HASH,
                0,
                self.next_epoch_state().cloned(),
            ),
            self.block().quorum_cert().certified_block().clone(),
        )
    }

    /// This function returns the vote data with a extension proof.
    /// Attention: this function itself does not verify the proof.
    fn vote_data_with_extension_proof(
        &self,
        new_tree: &InMemoryAccumulator<TransactionAccumulatorHasher>,
    ) -> VoteData {
        VoteData::new(
            self.block().gen_block_info(
                new_tree.root_hash(),
                new_tree.version(),
                self.next_epoch_state().cloned(),
            ),
            self.block().quorum_cert().certified_block().clone(),
        )
    }

    /// Generate vote data depends on the config.
    pub fn gen_vote_data(&self) -> anyhow::Result<VoteData> {
        if self.decoupled_execution {
            Ok(self.vote_data_ordering_only())
        } else {
            let proposed_block = self.block();
            let new_tree = self.accumulator_extension_proof().verify(
                proposed_block
                    .quorum_cert()
                    .certified_block()
                    .executed_state_id(),
            )?;
            Ok(self.vote_data_with_extension_proof(&new_tree))
        }
    }
}

impl Display for VoteProposal {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "VoteProposal[block: {}]", self.block,)
    }
}

/// Wraps a vote_proposal and its signature.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MaybeSignedVoteProposal {
    /// The vote proposal to be signed.
    pub vote_proposal: VoteProposal,

    /// The signature of this proposal's hash from the Execution Correctness service. It is
    /// an `Option` because the LEC can be configured to not sign the vote hash.
    pub signature: Option<Ed25519Signature>,
}

impl Deref for MaybeSignedVoteProposal {
    type Target = VoteProposal;

    fn deref(&self) -> &VoteProposal {
        &self.vote_proposal
    }
}
