// SPDX-License-Identifier: BUSL-1.1

use core_crypto::extrinsic::PALLET_GOVERNANCE;
use core_crypto::pallets::authority::AuthorityCall;
use core_crypto::pallets::governance::{
    voting_weight_group, GovAction, GovernanceCall, ProposalStatus, MAX_OPEN_PER_PROPOSER,
};

use crate::state::{now_ms, AppState};
use crate::wallet::Confirm;

pub const DISSOLVE_CONFIRMATION: &str = "dissolve-founder-authority";

impl AppState {
    /// The wallet's weight: its own history and its nodes'.
    pub async fn my_voting_weight(&self) -> u64 {
        let me = self.my_account().await;
        let chain = self.chain.read().await;
        voting_weight_group(
            &chain.contributions,
            &chain.identity.group_of(&me),
            now_ms(),
        )
    }

    pub async fn governance_propose(&self, action: GovAction) -> Result<(), String> {
        self.require_unlocked().await?;
        let me = self.my_account().await;
        {
            let chain = self.chain.read().await;
            let group = chain.identity.group_of(&me);
            if voting_weight_group(&chain.contributions, &group, now_ms()) == 0 {
                return Err("no voting weight yet: contribute compute for at least 7 days".into());
            }
            let mine_open = chain
                .governance
                .open_proposals()
                .filter(|p| p.proposer == me)
                .count();
            if mine_open >= MAX_OPEN_PER_PROPOSER {
                return Err("too many open proposals from this wallet".into());
            }
        }
        self.confirm(Confirm::Propose {
            action: format!("{action:?}"),
        })
        .await?;
        self.sign_and_submit_as_wallet(PALLET_GOVERNANCE, &GovernanceCall::Propose { action })
            .await?;
        Ok(())
    }

    pub async fn governance_vote(&self, id: u64, aye: bool) -> Result<(), String> {
        self.require_unlocked().await?;
        let me = self.my_account().await;
        {
            let chain = self.chain.read().await;
            let group = chain.identity.group_of(&me);
            if voting_weight_group(&chain.contributions, &group, now_ms()) == 0 {
                return Err("no voting weight yet: contribute compute for at least 7 days".into());
            }
            let Some(p) = chain.governance.proposals.get(&id) else {
                return Err("unknown proposal".into());
            };
            if p.status != ProposalStatus::Open || chain.head_height() >= p.voting_closes_at() {
                return Err("voting on this proposal is closed".into());
            }
            if p.votes.contains_key(&me) {
                return Err("this wallet already voted".into());
            }
        }
        self.confirm(Confirm::Vote { id, aye }).await?;
        self.sign_and_submit_as_wallet(PALLET_GOVERNANCE, &GovernanceCall::Vote { id, aye })
            .await?;
        Ok(())
    }

    pub async fn governance_enact(&self, id: u64) -> Result<(), String> {
        self.require_unlocked().await?;
        {
            let chain = self.chain.read().await;
            let Some(p) = chain.governance.proposals.get(&id) else {
                return Err("unknown proposal".into());
            };
            if p.status != ProposalStatus::Open {
                return Err("this proposal is already closed".into());
            }
            if chain.head_height() < p.enactable_at() {
                return Err(format!(
                    "enactable from block {} (head is {})",
                    p.enactable_at(),
                    chain.head_height()
                ));
            }
        }
        self.confirm(Confirm::Enact { id }).await?;
        self.sign_and_submit_as_wallet(PALLET_GOVERNANCE, &GovernanceCall::Enact { id })
            .await?;
        Ok(())
    }

    pub async fn bootstrap_dissolution(&self) {
        if !self.am_i_authority() {
            return;
        }
        let Ok(confirm) = std::env::var("NEPHERAL_DISSOLVE_AUTHORITY") else {
            return;
        };
        if confirm.trim() != DISSOLVE_CONFIRMATION {
            tracing::warn!(
                expected = DISSOLVE_CONFIRMATION,
                "NEPHERAL_DISSOLVE_AUTHORITY is set but does not match the confirmation phrase; ignored"
            );
            return;
        }
        let (dissolved, proven) = {
            let chain = self.chain.read().await;
            (
                chain.authority.is_dissolved(),
                chain.governance.oracle_replaced_by_vote,
            )
        };
        if dissolved {
            tracing::info!("Founder authority already dissolved");
            return;
        }
        if !proven {
            tracing::warn!(
                "Dissolution refused: governance has not yet replaced the sale oracle by vote"
            );
            return;
        }
        tracing::warn!("Submitting irreversible dissolution of the founder authority");
        self.queue_authority_signed(
            core_crypto::extrinsic::PALLET_AUTHORITY,
            &AuthorityCall::Dissolve,
        )
        .await;
    }
}
