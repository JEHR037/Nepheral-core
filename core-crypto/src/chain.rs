// SPDX-License-Identifier: BUSL-1.1
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::block::{Block, BlockBody, BlockHash, BlockHeader, LotteryResult};
use crate::extrinsic::{DecodedCall, Extrinsic, ExtrinsicError};
use crate::identity::PeerId;
use crate::pallets::authority::AuthorityCall;
use crate::pallets::balances::{BalancesPallet, BalancesStorage};
use crate::pallets::contributions::{ContributionsCall, ContributionsPallet, ContributionsStorage};
use crate::pallets::governance::{GovAction, GovernancePallet, GovernanceStorage};
use crate::pallets::identity::{IdentityCall, IdentityPallet, IdentityStorage};
use crate::pallets::nai::NaiCall;
use crate::pallets::nai::{NaiPallet, NaiStorage};
use crate::pallets::reputation::{ReputationPallet, ReputationStorage};
use crate::pallets::system::{SystemPallet, SystemStorage};
use crate::pallets::upgrade::{min_notice_ms, Activation, UpgradeCall, UpgradeStorage};
use crate::pallets::{ApplyCtx, Pallet, PalletError};
use crate::validators::ValidatorSet;

pub const NEPHERAL_TEAM_PUBKEY: [u8; 32] = [
    0x1f, 0x1b, 0x29, 0x54, 0x51, 0x0c, 0xeb, 0x84, 0x10, 0x14, 0x8e, 0x25, 0x7c, 0xf0, 0xa5, 0x09,
    0x14, 0xf5, 0x08, 0xaf, 0x5c, 0x01, 0x7f, 0xf2, 0x8a, 0x79, 0x7e, 0xe3, 0x4c, 0x92, 0xcb, 0xe2,
];

pub fn team_pubkey() -> [u8; 32] {
    parse_team_pubkey_env().unwrap_or(NEPHERAL_TEAM_PUBKEY)
}

fn parse_team_pubkey_env() -> Option<[u8; 32]> {
    let raw = option_env!("NEPHERAL_TEAM_PUBKEY")?.trim();
    if raw.is_empty() {
        return None;
    }
    let bytes = hex::decode(raw).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
}

pub const NAI_CREDITS_PER_LTC: u64 = crate::pallets::nai::NAI_CREDITS_PER_LTC;

pub const PRIORITY_SESSION_COST: u64 = crate::pallets::nai::PRIORITY_SESSION_COST;

pub const RPC_URL: &str = "https://rpc.nepheral.io";

pub type RedeemedCounts = crate::pallets::contributions::RedeemedCounts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainError {
    Pallet(PalletError),
    Extrinsic(ExtrinsicError),
    UnauthorizedAuthorityCall,
    BadBlockHeight,
    BadParentHash,
    BadBlockSignature,
    BlockContainsInvalidExtrinsic,
    LotteryDimensionMismatch,
    BadExtrinsicsRoot,
    BadLotteryRoot,
    BadStateRoot,
    BadTimestamp,
    BlockTooLarge,
    NoValidators,
    GovernanceNotProven,
    BadSlot,
    ReorgTooDeep,
    NotLonger,
    BadJustification,
    BelowFinality,
    /// The chain switched to rules this build does not implement. The node
    /// stops here rather than build on the old rules; installing the release
    /// for `version` lets it replay and continue.
    UpgradeRequired {
        version: u32,
    },
}

impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for ChainError {}

impl From<PalletError> for ChainError {
    fn from(e: PalletError) -> Self {
        ChainError::Pallet(e)
    }
}
impl From<ExtrinsicError> for ChainError {
    fn from(e: ExtrinsicError) -> Self {
        ChainError::Extrinsic(e)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainState {
    pub system: SystemStorage,
    pub balances: BalancesStorage,
    pub contributions: ContributionsStorage,
    pub nai: NaiStorage,
    #[serde(default)]
    pub authority: crate::pallets::authority::AuthorityStorage,
    #[serde(default)]
    pub economy: crate::pallets::economy::EconomyStorage,
    #[serde(default)]
    pub market: crate::pallets::market::MarketStorage,
    #[serde(default)]
    pub governance: GovernanceStorage,
    #[serde(default)]
    pub reputation: ReputationStorage,
    #[serde(default)]
    pub identity: IdentityStorage,
    #[serde(default)]
    pub upgrade: UpgradeStorage,

    #[serde(default)]
    pub blocks: Vec<Block>,
    /// The header this state was imported at, for a node that started from
    /// a verified snapshot (`crate::light`) instead of replaying from
    /// genesis. `blocks` then begins right after it.
    #[serde(default)]
    pub base: Option<BlockHeader>,

    #[serde(skip)]
    checkpoints: Vec<(u64, StateSnapshot)>,

    #[serde(skip)]
    recent_hashes: BTreeMap<u64, BlockHash>,
}

pub const TREASURY_GENESIS_AMOUNT: u64 = 1_000_000;

pub const MAX_BLOCK_EXTRINSICS: usize = 512;

pub const MAX_BLOCK_LOTTERY_RESULTS: usize = 256;

pub const MAX_BLOCK_BYTES: usize = 2 * 1024 * 1024;

pub const MAX_REORG_DEPTH: u64 = 64;

pub const CHECKPOINT_INTERVAL: u64 = 16;

pub const RECENT_HASHES: u64 = 1_024;

#[derive(Debug, Clone, Default)]
struct StateSnapshot {
    system: SystemStorage,
    balances: BalancesStorage,
    contributions: ContributionsStorage,
    nai: NaiStorage,
    authority: crate::pallets::authority::AuthorityStorage,
    economy: crate::pallets::economy::EconomyStorage,
    market: crate::pallets::market::MarketStorage,
    governance: GovernanceStorage,
    reputation: ReputationStorage,
    identity: IdentityStorage,
    upgrade: UpgradeStorage,
}

struct EnactTargets<'a> {
    nai: &'a mut NaiStorage,
    balances: &'a mut BalancesStorage,
    authority: &'a mut crate::pallets::authority::AuthorityStorage,
    economy: &'a mut crate::pallets::economy::EconomyStorage,
    upgrade: &'a mut UpgradeStorage,
}

impl ChainState {
    pub fn new() -> Self {
        Self::with_genesis_authority(team_pubkey())
    }

    #[cfg(test)]
    pub fn new_empty() -> Self {
        Self::default()
    }

    pub fn with_genesis_authority(pubkey: PeerId) -> Self {
        let mut state = Self::default();
        if pubkey != [0u8; 32] {
            state.balances.mint(pubkey, TREASURY_GENESIS_AMOUNT);
            state.nai.total_minted = state
                .nai
                .total_minted
                .saturating_add(TREASURY_GENESIS_AMOUNT);
            state.authority =
                crate::pallets::authority::AuthorityStorage::with_genesis(pubkey, Vec::new());
            tracing::info!(
                authority = %hex::encode(&pubkey[..8]),
                amount = TREASURY_GENESIS_AMOUNT,
                "Treasury genesis: minted N-AI + seeded validator set"
            );
        }
        state
    }

    fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            system: self.system.clone(),
            balances: self.balances.clone(),
            contributions: self.contributions.clone(),
            nai: self.nai.clone(),
            authority: self.authority.clone(),
            economy: self.economy.clone(),
            market: self.market.clone(),
            governance: self.governance.clone(),
            reputation: self.reputation.clone(),
            identity: self.identity.clone(),
            upgrade: self.upgrade.clone(),
        }
    }

    fn restore(&mut self, snap: StateSnapshot) {
        self.system = snap.system;
        self.balances = snap.balances;
        self.contributions = snap.contributions;
        self.nai = snap.nai;
        self.authority = snap.authority;
        self.economy = snap.economy;
        self.market = snap.market;
        self.governance = snap.governance;
        self.reputation = snap.reputation;
        self.identity = snap.identity;
        self.upgrade = snap.upgrade;
    }

    pub fn state_without_history(&self) -> ChainState {
        ChainState {
            system: self.system.clone(),
            balances: self.balances.clone(),
            contributions: self.contributions.clone(),
            nai: self.nai.clone(),
            authority: self.authority.clone(),
            economy: self.economy.clone(),
            market: self.market.clone(),
            governance: self.governance.clone(),
            reputation: self.reputation.clone(),
            identity: self.identity.clone(),
            upgrade: self.upgrade.clone(),
            blocks: Vec::new(),
            base: self.base.clone(),
            checkpoints: Vec::new(),
            recent_hashes: self.recent_hashes.clone(),
        }
    }

    pub fn balance_of(&self, peer: &PeerId) -> u64 {
        self.balances.balance_of(peer)
    }

    pub fn redeemable(&self, peer: &PeerId) -> (u64, u64) {
        self.contributions.redeemable(peer)
    }

    pub fn consumed_of(&self, peer: &PeerId) -> u64 {
        self.contributions.consumed_of(peer)
    }

    pub fn shares_of(&self, peer: &PeerId) -> u64 {
        self.contributions.shares_of(peer)
    }

    /// What the peer gave the network. See `ContributionsStorage::contribution_of`
    /// for why this is not `shares_of`.
    pub fn contribution_of(&self, peer: &PeerId) -> u64 {
        self.contributions.contribution_of(peer)
    }

    pub fn wins_of(&self, peer: &PeerId) -> u64 {
        self.contributions.wins_of(peer)
    }

    pub fn apply_extrinsic(
        &mut self,
        ext: &Extrinsic,
        block_height: u64,
        timestamp_ms: u64,
        author: &PeerId,
    ) -> Result<(), ChainError> {
        ext.verify_signature()?;

        let decoded = DecodedCall::decode(ext.pallet_idx, &ext.args)?;

        let founder_only = matches!(
            &decoded,
            DecodedCall::Contributions(ContributionsCall::RecordShare { .. })
                | DecodedCall::Contributions(ContributionsCall::RecordConsumption { .. })
                | DecodedCall::Nai(NaiCall::SetDirectSaleWallet { .. })
                | DecodedCall::Nai(NaiCall::SetChannelSessionCost { .. })
                | DecodedCall::Nai(NaiCall::SetDailyMintCap { .. })
                | DecodedCall::Nai(NaiCall::SetSaleOracle { .. })
                | DecodedCall::Authority(AuthorityCall::Add { .. })
                | DecodedCall::Authority(AuthorityCall::Dissolve)
                | DecodedCall::Economy(_)
                | DecodedCall::Upgrade(_)
        );
        if founder_only && !self.authority.is_founder(&ext.signer) {
            return Err(ChainError::UnauthorizedAuthorityCall);
        }
        if matches!(&decoded, DecodedCall::Authority(_))
            && !self.authority.is_authority_signer(&ext.signer)
        {
            return Err(ChainError::UnauthorizedAuthorityCall);
        }
        if matches!(&decoded, DecodedCall::Authority(AuthorityCall::Dissolve))
            && !self.governance.oracle_replaced_by_vote
        {
            return Err(ChainError::GovernanceNotProven);
        }

        if matches!(
            &decoded,
            DecodedCall::Nai(NaiCall::DirectSale { .. })
                | DecodedCall::Market(crate::pallets::market::MarketCall::Settle { .. })
                | DecodedCall::Market(crate::pallets::market::MarketCall::FailClaim { .. })
                | DecodedCall::Nai(NaiCall::SetSaleHealth { .. })
        ) && !self.authority.is_founder(&ext.signer)
            && self.nai.sale_oracle != Some(ext.signer)
        {
            return Err(ChainError::UnauthorizedAuthorityCall);
        }

        self.system.verify_and_bump_nonce(&ext.signer, ext.nonce)?;

        let decoded = match decoded {
            DecodedCall::Identity(call) => {
                // The founder's treasury is not anyone's node.
                if let IdentityCall::Delegate { node, .. } = &call {
                    if self.authority.is_founder(node) {
                        return Err(ChainError::UnauthorizedAuthorityCall);
                    }
                }
                IdentityPallet::apply(&mut self.identity, &mut self.balances, call, &ext.signer)?;
                self.sweep_delegated();
                self.authority.note_membership(block_height);
                return Ok(());
            }
            other => other,
        };

        // A node spends an inference of a session its wallet paid for, and
        // signs that with its own key: the pallet sees the wallet.
        let acting_for = match &decoded {
            DecodedCall::Nai(NaiCall::UseSession { session_token }) => {
                self.identity.wallet_of(&ext.signer).filter(|w| {
                    self.nai
                        .sessions
                        .get(session_token)
                        .is_some_and(|s| &s.owner == w)
                })
            }
            _ => None,
        };
        let signer = acting_for.unwrap_or(ext.signer);
        let rules_version = self.upgrade.active();

        let mut ctx = ApplyCtx {
            block_height,
            timestamp_ms,
            author,
            signer: &signer,
            system: &mut self.system,
            current_shares_per_credit: self.economy.current_shares_per_credit,
            identity: &self.identity,
            rules_version,
        };

        match decoded {
            // Applied above, before the context borrows the delegations.
            DecodedCall::Identity(_) => {}
            DecodedCall::Balances(call) => {
                let burned = match &call {
                    crate::pallets::balances::BalancesCall::Burn { amount } => *amount,
                    _ => 0,
                };
                BalancesPallet::apply(&mut self.balances, call, &mut ctx)?;
                // Counted with what sessions burn, so supply stays
                // minted - burned.
                self.nai.total_spent = self.nai.total_spent.saturating_add(burned);
            }
            DecodedCall::Contributions(call) => {
                let rateable = match &call {
                    ContributionsCall::SubmitReceipt { receipt }
                    | ContributionsCall::SubmitReceiptWithCustody { receipt, .. } => Some((
                        receipt.claim.query_digest,
                        receipt.claim.sender_pid,
                        receipt.claim.responder_pid,
                    )),
                    _ => None,
                };
                let minted = ContributionsPallet::apply_with_balances(
                    &mut self.contributions,
                    &mut self.balances,
                    call,
                    &mut ctx,
                )?;
                self.nai.total_minted = self.nai.total_minted.saturating_add(minted);
                if let Some((digest, sender, responder)) = rateable {
                    self.reputation
                        .register(digest, sender, responder, block_height);
                }
            }
            DecodedCall::Reputation(call) => {
                ReputationPallet::apply(&mut self.reputation, &self.contributions, call, &mut ctx)?;
            }
            DecodedCall::Finality(call) => match call {
                crate::finality::FinalityCall::Finalize { justification } => {
                    let j = justification;
                    if j.height <= ctx.system.finalized_height
                        || j.height >= block_height
                        || block_hash_at(&self.recent_hashes, &self.blocks, j.height)
                            != Some(j.block_hash)
                    {
                        return Err(ChainError::BadJustification);
                    }
                    j.verify(&self.authority.validators_at(j.height))
                        .map_err(|_| ChainError::BadJustification)?;
                    ctx.system.finalized_height = j.height;
                    ctx.system.finalized_hash = j.block_hash;
                    self.authority.prune_history(j.height);
                }
                crate::finality::FinalityCall::ReportEquivocation { proof } => {
                    let offender = proof.offender().ok_or(ChainError::BadJustification)?;
                    self.authority
                        .punish_equivocation(offender, proof.first.height)?;
                }
            },
            DecodedCall::Nai(call) => {
                NaiPallet::apply_with_balances(&mut self.nai, &mut self.balances, call, &mut ctx)?;
            }
            DecodedCall::Authority(call) => {
                crate::pallets::authority::AuthorityPallet::apply(
                    &mut self.authority,
                    call,
                    &mut ctx,
                )?;
            }
            DecodedCall::Economy(call) => {
                crate::pallets::economy::EconomyPallet::apply(&mut self.economy, call, &mut ctx)?;
            }
            DecodedCall::Market(call) => {
                crate::pallets::market::MarketPallet::apply_with_balances(
                    &mut self.market,
                    &mut self.balances,
                    &self.contributions,
                    call,
                    &mut ctx,
                )?;
            }
            DecodedCall::Governance(call) => {
                let founder_active = !self.authority.is_dissolved();
                if let Some((id, action)) = GovernancePallet::apply(
                    &mut self.governance,
                    &self.contributions,
                    founder_active,
                    call,
                    &mut ctx,
                )? {
                    let enacted = Self::enact(
                        EnactTargets {
                            nai: &mut self.nai,
                            balances: &mut self.balances,
                            authority: &mut self.authority,
                            economy: &mut self.economy,
                            upgrade: &mut self.upgrade,
                        },
                        action,
                        &mut ctx,
                    );
                    GovernancePallet::finish(&mut self.governance, id, block_height, enacted);
                }
            }
            DecodedCall::Upgrade(call) => match call {
                UpgradeCall::Schedule {
                    version,
                    activate_at_ms,
                    release,
                } => self.upgrade.schedule(
                    version,
                    activate_at_ms,
                    release,
                    timestamp_ms,
                    block_height,
                    min_notice_ms(crate::network::active()),
                )?,
                UpgradeCall::Cancel => self.upgrade.cancel()?,
            },
        }
        self.sweep_delegated();
        self.authority.note_membership(block_height);
        Ok(())
    }

    /// Moves anything a delegated node holds to its wallet - redeemed
    /// earnings, a transfer, a refund - so a node key never holds funds. Runs
    /// after every extrinsic; with no delegations it does nothing, which is
    /// what keeps earlier history reconciling to the same roots.
    fn sweep_delegated(&mut self) {
        for (node, wallet) in &self.identity.wallet_of {
            let held = self.balances.balance_of(node);
            if held > 0 && self.balances.burn_checked(node, held).is_ok() {
                self.balances.mint(*wallet, held);
            }
        }
    }

    /// The account that owns what `peer` does: its wallet when `peer` is a
    /// delegated node, `peer` itself otherwise.
    pub fn account_of(&self, peer: &PeerId) -> PeerId {
        self.identity.account_of(peer)
    }

    fn enact(targets: EnactTargets<'_>, action: GovAction, ctx: &mut ApplyCtx<'_>) -> bool {
        let mut nai = targets.nai.clone();
        let mut balances = targets.balances.clone();
        let mut authority = targets.authority.clone();
        let mut economy = targets.economy.clone();
        let mut upgrade = targets.upgrade.clone();
        let result = match action {
            GovAction::SetSaleOracle { pubkey } => NaiPallet::apply_with_balances(
                &mut nai,
                &mut balances,
                NaiCall::SetSaleOracle { pubkey },
                ctx,
            ),
            GovAction::SetDirectSaleWallet { address } => NaiPallet::apply_with_balances(
                &mut nai,
                &mut balances,
                NaiCall::SetDirectSaleWallet { address },
                ctx,
            ),
            GovAction::SetDailyMintCap { cap } => NaiPallet::apply_with_balances(
                &mut nai,
                &mut balances,
                NaiCall::SetDailyMintCap { cap },
                ctx,
            ),
            GovAction::SetChannelSessionCost { channel, cost } => NaiPallet::apply_with_balances(
                &mut nai,
                &mut balances,
                NaiCall::SetChannelSessionCost { channel, cost },
                ctx,
            ),
            GovAction::SetEmissionTarget {
                target_credits_per_hour,
            } => crate::pallets::economy::EconomyPallet::apply(
                &mut economy,
                crate::pallets::economy::EconomyCall::SetEmissionTarget {
                    target_credits_per_hour,
                },
                ctx,
            ),
            GovAction::AddPartner { .. } | GovAction::RemovePartner { .. }
                if !authority.is_dissolved() =>
            {
                Err(PalletError::Unauthorized)
            }
            GovAction::AddPartner {
                pubkey,
                channels,
                attestation_hash,
            } => authority.add_member(pubkey, channels, attestation_hash, ctx.block_height),
            GovAction::RemovePartner { pubkey } => authority.remove_member(&pubkey),
            // While the founder is active it schedules upgrades itself.
            GovAction::ScheduleUpgrade { .. } | GovAction::CancelUpgrade
                if !authority.is_dissolved() =>
            {
                Err(PalletError::Unauthorized)
            }
            // The notice counts from enactment, not from the proposal.
            GovAction::ScheduleUpgrade {
                version,
                activate_at_ms,
                release,
            } => upgrade.schedule(
                version,
                activate_at_ms,
                release,
                ctx.timestamp_ms,
                ctx.block_height,
                min_notice_ms(crate::network::active()),
            ),
            GovAction::CancelUpgrade => upgrade.cancel(),
        };
        if result.is_err() {
            return false;
        }
        *targets.nai = nai;
        *targets.balances = balances;
        *targets.authority = authority;
        *targets.economy = economy;
        *targets.upgrade = upgrade;
        true
    }

    pub fn genesis_block() -> Block {
        Block {
            header: BlockHeader {
                version: 1,
                height: 0,
                slot: 0,
                parent_hash: crate::network::active().chain_id(),
                state_root: [0u8; 32],
                extrinsics_root: [0u8; 32],
                lottery_root: [0u8; 32],
                author: [0u8; 32],
                signature: [0u8; 64],
                timestamp_ms: 0,
            },
            body: BlockBody {
                extrinsics: Vec::new(),
                lottery_results: Vec::new(),
            },
        }
    }

    /// The newest header: the last block's, else the imported base's.
    fn head_header(&self) -> Option<&BlockHeader> {
        self.blocks.last().map(|b| &b.header).or(self.base.as_ref())
    }

    pub fn head_hash(&self) -> BlockHash {
        match self.head_header() {
            Some(h) => h.block_hash(),
            None => Self::genesis_block().header.block_hash(),
        }
    }

    pub fn head_height(&self) -> u64 {
        self.head_header().map(|h| h.height).unwrap_or(0)
    }

    /// A state imported at `header` (see `crate::light`): it continues from
    /// there with the next block, keeping no history before it. The caller
    /// has checked that `self` is the state `header` commits to.
    pub fn imported_at(mut self, header: BlockHeader) -> Self {
        self.blocks.clear();
        self.checkpoints.clear();
        self.recent_hashes.clear();
        self.recent_hashes
            .insert(header.height, header.block_hash());
        self.base = Some(header);
        self
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn ext_count(&self) -> usize {
        self.blocks.iter().map(|b| b.body.extrinsics.len()).sum()
    }

    pub fn head_timestamp_ms(&self) -> Option<u64> {
        self.head_header().map(|h| h.timestamp_ms)
    }

    pub fn state_root(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        const SEP: &[u8] = b"||";
        h.update(SystemPallet::pallet_root(&self.system));
        h.update(SEP);
        h.update(BalancesPallet::pallet_root(&self.balances));
        h.update(SEP);
        h.update(ContributionsPallet::pallet_root(&self.contributions));
        h.update(SEP);
        h.update(NaiPallet::pallet_root(&self.nai));
        h.update(SEP);
        h.update(crate::pallets::market::MarketPallet::pallet_root(
            &self.market,
        ));
        h.update(SEP);
        h.update(crate::pallets::authority::AuthorityPallet::pallet_root(
            &self.authority,
        ));
        h.update(SEP);
        h.update(crate::pallets::economy::EconomyPallet::pallet_root(
            &self.economy,
        ));
        h.update(SEP);
        h.update(GovernancePallet::pallet_root(&self.governance));
        h.update(SEP);
        h.update(ReputationPallet::pallet_root(&self.reputation));
        // Joins the root with the first delegation, so the history written
        // before the pallet existed keeps the roots it was sealed with.
        if !self.identity.is_empty() {
            h.update(SEP);
            h.update(IdentityPallet::pallet_root(&self.identity));
        }
        // The same for upgrades: from the first one scheduled.
        if !self.upgrade.is_default() {
            h.update(SEP);
            h.update(self.upgrade.pallet_root());
        }
        h.finalize().into()
    }

    pub fn validator_set(&self) -> ValidatorSet {
        self.authority.to_validator_set()
    }

    pub fn apply_block(&mut self, block: Block) -> Result<BlockHash, ChainError> {
        let validators = self.validator_set();
        if validators.is_empty() {
            return Err(ChainError::NoValidators);
        }
        validators
            .verify_block_signature(&block.header)
            .map_err(|_| ChainError::BadBlockSignature)?;
        self.apply_block_checked(block, true)
    }

    pub fn apply_block_unverified(&mut self, block: Block) -> Result<BlockHash, ChainError> {
        self.apply_block_checked(block, false)
    }

    pub fn check_block_shape(block: &Block) -> Result<(), ChainError> {
        if block.body.extrinsics.len() > MAX_BLOCK_EXTRINSICS
            || block.body.lottery_results.len() > MAX_BLOCK_LOTTERY_RESULTS
        {
            return Err(ChainError::BlockTooLarge);
        }
        let body_bytes: usize = block
            .body
            .extrinsics
            .iter()
            .map(|e| e.args.len() + 128)
            .sum::<usize>()
            + block.body.lottery_results.len() * 160;
        if body_bytes > MAX_BLOCK_BYTES {
            return Err(ChainError::BlockTooLarge);
        }
        if block.extrinsics_root() != block.header.extrinsics_root {
            return Err(ChainError::BadExtrinsicsRoot);
        }
        if block.lottery_root() != block.header.lottery_root {
            return Err(ChainError::BadLotteryRoot);
        }
        if !block.header.slot_matches_timestamp() {
            return Err(ChainError::BadTimestamp);
        }
        Ok(())
    }

    fn apply_block_checked(
        &mut self,
        block: Block,
        check_state_root: bool,
    ) -> Result<BlockHash, ChainError> {
        let expected_height = self.head_height() + 1;
        if block.header.height != expected_height {
            return Err(ChainError::BadBlockHeight);
        }
        if block.header.parent_hash != self.head_hash() {
            return Err(ChainError::BadParentHash);
        }
        Self::check_block_shape(&block)?;
        if let Some(parent) = self.head_header() {
            if block.header.timestamp_ms < parent.timestamp_ms {
                return Err(ChainError::BadTimestamp);
            }
            if block.header.slot <= parent.slot {
                return Err(ChainError::BadSlot);
            }
        }

        let snapshot = self.snapshot();
        if self.blocks.is_empty() && self.checkpoints.is_empty() {
            self.checkpoints
                .push((self.head_height(), snapshot.clone()));
        }

        let block_height = block.header.height;
        let timestamp_ms = block.header.timestamp_ms;
        let author = block.header.author;

        // Before anything else in the block, so its extrinsics run under the
        // rules in force from it. Nothing has been changed yet when it bails.
        match self.upgrade.activation_for(timestamp_ms) {
            Activation::None => {}
            Activation::Activate { version } => {
                self.migrate_to(version);
                self.upgrade.activate(version, block_height);
                tracing::info!(version, height = block_height, "Chain rules upgraded");
            }
            Activation::Unsupported { version } => {
                return Err(ChainError::UpgradeRequired { version });
            }
        }

        NaiPallet::drain_due_changes(&mut self.nai, block_height);
        self.reputation.prune(block_height);

        for ext in &block.body.extrinsics {
            if let Err(e) = self.apply_extrinsic(ext, block_height, timestamp_ms, &author) {
                self.restore(snapshot);
                tracing::warn!(error = ?e, "Block contained invalid extrinsic — rolled back");
                return Err(ChainError::BlockContainsInvalidExtrinsic);
            }
        }

        for result in &block.body.lottery_results {
            if !result.dimension_matches_inputs() {
                self.restore(snapshot);
                tracing::warn!(
                    winner_fp = %hex::encode(&result.winner[..8]),
                    declared_dim = result.dimension,
                    "Block rejected: LotteryResult.dimension does not match compute_dimension(model, tokens, ms)"
                );
                return Err(ChainError::LotteryDimensionMismatch);
            }
            self.contributions
                .record_lottery_win(result.winner, result.dimension);
        }

        let block_share_equivs: u64 = {
            let extrinsics_share: u64 = block
                .body
                .extrinsics
                .iter()
                .filter_map(|ext| DecodedCall::decode(ext.pallet_idx, &ext.args).ok())
                .map(|d| match d {
                    DecodedCall::Contributions(ContributionsCall::RecordShare { .. }) => 1,
                    DecodedCall::Contributions(ContributionsCall::SubmitReceipt { receipt }) => {
                        receipt.claim.dimension()
                    }
                    DecodedCall::Contributions(ContributionsCall::SubmitReceiptWithCustody {
                        receipt,
                        custody,
                    }) => receipt.claim.dimension().saturating_add(
                        custody.custodians.len() as u64
                            / crate::pallets::contributions::CUSTODY_PER_SHARE,
                    ),
                    _ => 0,
                })
                .sum();
            let lottery_dim: u64 = block.body.lottery_results.iter().map(|r| r.dimension).sum();
            extrinsics_share.saturating_add(lottery_dim)
        };
        // Timestamps, not heights: the producer skips empty slots, so a block
        // count says nothing about how much time the work took.
        self.economy
            .record_block_activity(timestamp_ms, block_share_equivs);
        if self.economy.adjustment_due(timestamp_ms) {
            self.economy.recompute_ratio(timestamp_ms);
            self.economy.last_adjustment_ms = timestamp_ms;
        }

        self.system.block_height = block_height;
        self.system.current_slot = block.header.slot;

        if check_state_root && self.state_root() != block.header.state_root {
            self.restore(snapshot);
            tracing::warn!(
                height = block_height,
                "Block rejected: state root after applying the body does not match the header"
            );
            return Err(ChainError::BadStateRoot);
        }

        let hash = block.header.block_hash();
        self.blocks.push(block);
        self.recent_hashes.retain(|h, _| *h < block_height);
        self.recent_hashes.insert(block_height, hash);
        let keep_from = block_height.saturating_sub(RECENT_HASHES);
        while let Some((&h, _)) = self.recent_hashes.first_key_value() {
            if h >= keep_from {
                break;
            }
            self.recent_hashes.pop_first();
        }
        if block_height.is_multiple_of(CHECKPOINT_INTERVAL) {
            self.checkpoints.push((block_height, self.snapshot()));
            let keep_from = block_height.saturating_sub(MAX_REORG_DEPTH + CHECKPOINT_INTERVAL);
            let first_kept = self
                .checkpoints
                .iter()
                .rposition(|(h, _)| *h <= keep_from)
                .unwrap_or(0);
            self.checkpoints.drain(..first_kept);
        }
        Ok(hash)
    }

    /// One-off state changes that come with a new version of the rules.
    /// None yet: version 2 only changes how calls are checked.
    fn migrate_to(&mut self, version: u32) {
        let _ = version;
    }

    /// The version of the chain rules in force.
    pub fn rules_version(&self) -> u32 {
        self.upgrade.active()
    }

    pub fn hash_at(&self, height: u64) -> Option<BlockHash> {
        block_hash_at(&self.recent_hashes, &self.blocks, height)
    }

    fn rewound_to(&self, height: u64) -> Result<ChainState, ChainError> {
        let Some((cp_height, cp)) = self
            .checkpoints
            .iter()
            .rev()
            .find(|(h, _)| *h <= height)
            .cloned()
        else {
            return Err(ChainError::ReorgTooDeep);
        };
        let mut base = self.state_without_history();
        base.restore(cp);
        base.recent_hashes.retain(|h, _| *h <= cp_height);
        base.checkpoints = self
            .checkpoints
            .iter()
            .filter(|(h, _)| *h <= cp_height)
            .cloned()
            .collect();
        // An imported base is the anchor itself; it has no block to push.
        let at_base = self.base.as_ref().is_some_and(|h| h.height == cp_height);
        if cp_height > 0 && !at_base {
            let anchor = self
                .block_at(cp_height)
                .cloned()
                .ok_or(ChainError::ReorgTooDeep)?;
            base.blocks.push(anchor);
        }
        for h in cp_height + 1..=height {
            let block = self.block_at(h).cloned().ok_or(ChainError::ReorgTooDeep)?;
            base.apply_block_checked(block, true)?;
        }
        Ok(base)
    }

    fn block_at(&self, height: u64) -> Option<&Block> {
        let first = self.blocks.first()?.header.height;
        self.blocks.get(height.checked_sub(first)? as usize)
    }

    pub fn finalized(&self) -> (u64, BlockHash) {
        (self.system.finalized_height, self.system.finalized_hash)
    }

    pub fn try_reorg(
        &mut self,
        blocks: Vec<Block>,
        observed_final: u64,
    ) -> Result<Vec<Extrinsic>, ChainError> {
        let Some(first) = blocks.first() else {
            return Err(ChainError::BadBlockHeight);
        };
        let fork = first.header.height.saturating_sub(1);
        if fork < self.system.finalized_height.max(observed_final) {
            return Err(ChainError::BelowFinality);
        }
        let head = self.head_height();
        if fork > head || self.hash_at(fork) != Some(first.header.parent_hash) {
            return Err(ChainError::BadParentHash);
        }
        if fork + MAX_REORG_DEPTH < head {
            return Err(ChainError::ReorgTooDeep);
        }
        let new_head = fork + blocks.len() as u64;
        if new_head <= head {
            return Err(ChainError::NotLonger);
        }
        let mut candidate = self.rewound_to(fork)?;
        for block in blocks {
            candidate.apply_block(block)?;
        }
        let orphaned: Vec<Extrinsic> = self
            .blocks
            .iter()
            .filter(|b| b.header.height > fork)
            .flat_map(|b| b.body.extrinsics.iter().cloned())
            .collect();
        let start = candidate
            .blocks
            .first()
            .map(|b| b.header.height)
            .unwrap_or(fork + 1);
        let keep = self
            .blocks
            .iter()
            .position(|b| b.header.height >= start)
            .unwrap_or(self.blocks.len());
        self.blocks.truncate(keep);
        self.blocks.append(&mut candidate.blocks);
        let snap = candidate.snapshot();
        self.restore(snap);
        self.recent_hashes = candidate.recent_hashes;
        self.checkpoints = candidate.checkpoints;
        Ok(orphaned)
    }

    pub fn serialize_blocks(&self) -> Vec<u8> {
        self.serialize_blocks_after(0)
    }

    pub fn serialize_blocks_after(&self, height: u64) -> Vec<u8> {
        let mut out = Vec::new();
        for block in self.blocks.iter().filter(|b| b.header.height > height) {
            let bytes = crate::wire::encode(block);
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(&bytes);
        }
        out
    }

    pub fn replay_blocks(blocks: Vec<Block>) -> Self {
        let mut state = Self::new();
        for block in blocks {
            if let Err(e) = state.apply_block(block) {
                tracing::warn!(error = ?e, "Stopping block replay at first failure");
                break;
            }
        }
        state
    }

    pub fn deserialize_blocks(data: &[u8]) -> Option<Vec<Block>> {
        let mut blocks = Vec::new();
        let mut rest = data;
        while !rest.is_empty() {
            let (len, tail) = rest.split_at_checked(4)?;
            let len = u32::from_be_bytes(len.try_into().ok()?) as usize;
            let (body, tail) = tail.split_at_checked(len)?;
            blocks.push(postcard::from_bytes(body).ok()?);
            rest = tail;
        }
        Some(blocks)
    }

    pub fn history_of<'a>(&'a self, peer: &'a PeerId) -> Vec<(u64, &'a Extrinsic)> {
        let mut out = Vec::new();
        for block in &self.blocks {
            for ext in &block.body.extrinsics {
                if &ext.signer == peer || ext_involves_peer(ext, peer) {
                    out.push((block.header.height, ext));
                }
            }
        }
        out
    }

    pub fn lottery_count(&self) -> usize {
        self.blocks
            .iter()
            .map(|b| b.body.lottery_results.len())
            .sum()
    }

    pub fn last_win_of<'a>(&'a self, peer: &'a PeerId) -> Option<&'a LotteryResult> {
        for block in self.blocks.iter().rev() {
            for r in block.body.lottery_results.iter().rev() {
                if &r.winner == peer {
                    return Some(r);
                }
            }
        }
        None
    }
}

fn block_hash_at(
    recent: &BTreeMap<u64, BlockHash>,
    blocks: &[Block],
    height: u64,
) -> Option<BlockHash> {
    if height == 0 {
        return Some(ChainState::genesis_block().header.block_hash());
    }
    if let Some(h) = recent.get(&height) {
        return Some(*h);
    }
    let first = blocks.first()?.header.height;
    blocks
        .get(height.checked_sub(first)? as usize)
        .map(|b| b.header.block_hash())
}

fn ext_involves_peer(ext: &Extrinsic, peer: &PeerId) -> bool {
    use crate::pallets::balances::BalancesCall;
    use crate::pallets::nai::NaiCall;

    let Ok(decoded) = DecodedCall::decode(ext.pallet_idx, &ext.args) else {
        return false;
    };
    match decoded {
        DecodedCall::Balances(BalancesCall::Transfer { to, .. }) => &to == peer,
        DecodedCall::Balances(BalancesCall::Burn { .. }) => false,
        DecodedCall::Contributions(ContributionsCall::Redeem { receiver, .. }) => &receiver == peer,
        DecodedCall::Contributions(ContributionsCall::RecordShare { peer: p }) => &p == peer,
        DecodedCall::Contributions(ContributionsCall::RecordConsumption { peer: p }) => &p == peer,
        DecodedCall::Contributions(ContributionsCall::SubmitReceipt { receipt }) => {
            &receipt.claim.responder_pid == peer || &receipt.claim.sender_pid == peer
        }
        DecodedCall::Contributions(ContributionsCall::SubmitReceiptWithCustody {
            receipt,
            custody,
        }) => {
            &receipt.claim.responder_pid == peer
                || &receipt.claim.sender_pid == peer
                || custody.custodians.contains(peer)
        }
        DecodedCall::Nai(NaiCall::DirectSale { receiver, .. }) => &receiver == peer,
        DecodedCall::Nai(NaiCall::Spend { .. }) => false,
        DecodedCall::Nai(NaiCall::UseSession { .. }) => false,
        DecodedCall::Nai(NaiCall::OpenChannelSession { .. }) => false,
        DecodedCall::Nai(NaiCall::SetDirectSaleWallet { .. }) => false,
        DecodedCall::Nai(NaiCall::SetChannelSessionCost { .. }) => false,
        DecodedCall::Nai(NaiCall::SetDailyMintCap { .. }) => false,
        DecodedCall::Nai(NaiCall::SetSaleOracle { .. }) => false,
        DecodedCall::Nai(NaiCall::SetSaleHealth { .. }) => false,
        DecodedCall::Identity(call) => match call {
            IdentityCall::Delegate { node, .. } | IdentityCall::Revoke { node } => &node == peer,
        },
        DecodedCall::Authority(call) => match call {
            crate::pallets::authority::AuthorityCall::Add { pubkey, .. } => &pubkey == peer,
            crate::pallets::authority::AuthorityCall::Remove { pubkey } => &pubkey == peer,
            crate::pallets::authority::AuthorityCall::SetChannels { pubkey, .. } => &pubkey == peer,
            crate::pallets::authority::AuthorityCall::RotateKey {
                old_pubkey,
                new_pubkey,
            } => &old_pubkey == peer || &new_pubkey == peer,
            crate::pallets::authority::AuthorityCall::Dissolve => false,
        },
        DecodedCall::Governance(_) => false,
        DecodedCall::Reputation(_) => false,
        DecodedCall::Finality(_) => false,
        DecodedCall::Upgrade(_) => false,
        DecodedCall::Economy(_) => false,
        DecodedCall::Market(call) => {
            use crate::pallets::market::MarketCall;
            match call {
                MarketCall::Settle { buyer, .. } => &buyer == peer,
                MarketCall::ClaimFill { buyer, .. } => &buyer == peer,
                MarketCall::FailClaim { .. } => false,
                MarketCall::PostAsk { .. }
                | MarketCall::CancelAsk { .. }
                | MarketCall::PostBid { .. }
                | MarketCall::CancelBid { .. } => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extrinsic::{PALLET_BALANCES, PALLET_CONTRIBUTIONS};
    use crate::pallets::balances::BalancesCall;
    use ed25519_dalek::SigningKey;
    use rand::RngCore;

    fn fresh_signer() -> (SigningKey, PeerId) {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let sk = SigningKey::from_bytes(&bytes);
        let pid: PeerId = sk.verifying_key().to_bytes();
        (sk, pid)
    }

    fn direct_sale_ext(
        sk: &SigningKey,
        signer: PeerId,
        receiver: PeerId,
        nonce: u64,
        payment_ref: &str,
    ) -> Extrinsic {
        Extrinsic::sign(
            sk,
            crate::extrinsic::PALLET_NAI,
            &crate::pallets::nai::NaiCall::DirectSale {
                receiver,
                credits: 100,
                payment_ref: payment_ref.to_string(),
            },
            signer,
            nonce,
        )
    }

    #[test]
    fn direct_sale_from_unknown_key_is_rejected() {
        let (_, auth_pid) = fresh_signer();
        let (sk_rando, rando_pid) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        let ext = direct_sale_ext(&sk_rando, rando_pid, rando_pid, 1, "tx-a");
        let err = s.apply_extrinsic(&ext, 1, 0, &auth_pid).unwrap_err();
        assert!(matches!(err, ChainError::UnauthorizedAuthorityCall));
        assert_eq!(s.balance_of(&rando_pid), 0);
    }

    #[test]
    fn direct_sale_from_designated_sale_oracle_is_accepted() {
        let (sk_auth, auth_pid) = fresh_signer();
        let (sk_oracle, oracle_pid) = fresh_signer();
        let (_, buyer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);

        let set = Extrinsic::sign(
            &sk_auth,
            crate::extrinsic::PALLET_NAI,
            &crate::pallets::nai::NaiCall::SetSaleOracle {
                pubkey: Some(oracle_pid),
            },
            auth_pid,
            1,
        );
        s.apply_extrinsic(&set, 1, 0, &auth_pid).unwrap();
        assert_eq!(s.nai.sale_oracle, Some(oracle_pid));

        let sale = direct_sale_ext(&sk_oracle, oracle_pid, buyer, 1, "tx-b");
        s.apply_extrinsic(&sale, 2, 0, &auth_pid).unwrap();
        assert_eq!(s.balance_of(&buyer), 100);
    }

    #[test]
    fn clearing_the_sale_oracle_revokes_its_minting_right() {
        let (sk_auth, auth_pid) = fresh_signer();
        let (sk_oracle, oracle_pid) = fresh_signer();
        let (_, buyer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        s.nai.sale_oracle = Some(oracle_pid);

        let revoke = Extrinsic::sign(
            &sk_auth,
            crate::extrinsic::PALLET_NAI,
            &crate::pallets::nai::NaiCall::SetSaleOracle { pubkey: None },
            auth_pid,
            1,
        );
        s.apply_extrinsic(&revoke, 1, 0, &auth_pid).unwrap();

        let sale = direct_sale_ext(&sk_oracle, oracle_pid, buyer, 1, "tx-c");
        let err = s.apply_extrinsic(&sale, 2, 0, &auth_pid).unwrap_err();
        assert!(matches!(err, ChainError::UnauthorizedAuthorityCall));
        assert_eq!(s.balance_of(&buyer), 0);
    }

    #[test]
    fn a_non_authority_cannot_appoint_itself_sale_oracle() {
        let (_, auth_pid) = fresh_signer();
        let (sk_rando, rando_pid) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        let ext = Extrinsic::sign(
            &sk_rando,
            crate::extrinsic::PALLET_NAI,
            &crate::pallets::nai::NaiCall::SetSaleOracle {
                pubkey: Some(rando_pid),
            },
            rando_pid,
            1,
        );
        let err = s.apply_extrinsic(&ext, 1, 0, &auth_pid).unwrap_err();
        assert!(matches!(err, ChainError::UnauthorizedAuthorityCall));
        assert_eq!(s.nai.sale_oracle, None);
    }

    #[test]
    fn an_authority_can_still_submit_direct_sale_without_an_oracle() {
        let (sk_auth, auth_pid) = fresh_signer();
        let (_, buyer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        let sale = direct_sale_ext(&sk_auth, auth_pid, buyer, 1, "tx-d");
        s.apply_extrinsic(&sale, 1, 0, &auth_pid).unwrap();
        assert_eq!(s.balance_of(&buyer), 100);
    }

    const NOW_MS: u64 = 1_700_000_000_000;

    /// Gives `peer` the history the market gate requires, so these tests
    /// exercise the market rules rather than the standing rules.
    fn grant_standing(s: &mut ChainState, peer: PeerId) {
        use crate::pallets::market::{MIN_DAYS_ACTIVE, MIN_SHARES, MIN_WINS};
        s.contributions
            .first_seen_ms
            .insert(peer, NOW_MS - (MIN_DAYS_ACTIVE + 1) * 24 * 60 * 60 * 1_000);
        s.contributions.share_counts.insert(peer, MIN_SHARES);
        s.contributions.wins_by_peer.insert(peer, MIN_WINS);
    }

    fn market_ext(
        sk: &SigningKey,
        signer: PeerId,
        call: crate::pallets::market::MarketCall,
        nonce: u64,
    ) -> Extrinsic {
        Extrinsic::sign(sk, crate::extrinsic::PALLET_MARKET, &call, signer, nonce)
    }

    /// Posts an ask from `seller` so there is something to settle.
    fn seed_ask(s: &mut ChainState, sk: &SigningKey, seller: PeerId) {
        use crate::pallets::market::MarketCall;
        s.balances.mint(seller, 1_000);
        grant_standing(s, seller);
        let post = market_ext(
            sk,
            seller,
            MarketCall::PostAsk {
                credits: 400,
                ask_sats: 8_000_000,
                ltc_address: "ltc1qseller".to_string(),
            },
            1,
        );
        s.apply_extrinsic(&post, 1, NOW_MS, &seller).unwrap();
        assert_eq!(s.market.escrowed, 400);
    }

    /// Records the buyer's claim, which `Settle` requires.
    fn seed_claim(s: &mut ChainState, sk_buyer: &SigningKey, buyer: PeerId, author: &PeerId) {
        use crate::pallets::market::MarketCall;
        grant_standing(s, buyer);
        let claim = market_ext(
            sk_buyer,
            buyer,
            MarketCall::ClaimFill {
                ask_id: 0,
                buyer,
                payment_ref: "ltc-market-tx".to_string(),
            },
            1,
        );
        s.apply_extrinsic(&claim, 2, NOW_MS, author).unwrap();
    }

    fn settle_ext(sk: &SigningKey, signer: PeerId, buyer: PeerId, nonce: u64) -> Extrinsic {
        market_ext(
            sk,
            signer,
            crate::pallets::market::MarketCall::Settle {
                id: 0,
                buyer,
                payment_ref: "ltc-market-tx".to_string(),
            },
            nonce,
        )
    }

    #[test]
    fn a_seller_cannot_cancel_an_ask_while_a_buyer_claim_is_pending() {
        use crate::pallets::market::MarketCall;
        let (_, auth_pid) = fresh_signer();
        let (sk_seller, seller) = fresh_signer();
        let (sk_buyer, buyer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        seed_ask(&mut s, &sk_seller, seller);
        seed_claim(&mut s, &sk_buyer, buyer, &auth_pid);
        let cancel = market_ext(&sk_seller, seller, MarketCall::CancelAsk { id: 0 }, 2);
        assert!(s.apply_extrinsic(&cancel, 3, NOW_MS, &auth_pid).is_err());
        assert_eq!(s.market.escrowed, 400);
        let (sk_other, other) = fresh_signer();
        grant_standing(&mut s, other);
        let second = market_ext(
            &sk_other,
            other,
            MarketCall::ClaimFill {
                ask_id: 0,
                buyer: other,
                payment_ref: "another-tx".to_string(),
            },
            1,
        );
        assert!(s.apply_extrinsic(&second, 3, NOW_MS, &auth_pid).is_err());
    }

    fn receipt_ext(
        responder: &SigningKey,
        sender_sk: &SigningKey,
        digest: [u8; 32],
        nonce: u64,
    ) -> Extrinsic {
        let sender = sender_sk.verifying_key().to_bytes();
        let claim = crate::receipts::ServiceClaim::sign(
            responder,
            digest,
            sender,
            NOW_MS / 3_600_000,
            [0u8; 32],
            8,
            400,
            4_000,
        );
        let receipt = crate::receipts::ServiceReceipt::countersign(claim, sender_sk);
        Extrinsic::sign(
            sender_sk,
            PALLET_CONTRIBUTIONS,
            &ContributionsCall::SubmitReceipt {
                receipt: Box::new(receipt),
            },
            sender,
            nonce,
        )
    }

    #[test]
    fn a_countersigned_receipt_credits_the_responder_once() {
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, resp) = fresh_signer();
        let (sk_sender, sender) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        let ext = receipt_ext(&sk_resp, &sk_sender, [5u8; 32], 1);
        s.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).unwrap();
        assert_eq!(s.contributions.wins_of(&resp), 1);
        assert_eq!(s.consumed_of(&sender), 1);
        assert!(s.contributions.redeemable_lottery_dimension(&resp) > 0);
        let replay = receipt_ext(&sk_resp, &sk_sender, [5u8; 32], 2);
        assert!(s.apply_extrinsic(&replay, 2, NOW_MS, &auth_pid).is_err());
        assert_eq!(s.contributions.wins_of(&resp), 1);
    }

    fn custody_receipt_ext(
        responder: &SigningKey,
        sender_sk: &SigningKey,
        digest: [u8; 32],
        custodians: Vec<PeerId>,
        sign_custody_with: Option<&SigningKey>,
        nonce: u64,
    ) -> Extrinsic {
        let sender = sender_sk.verifying_key().to_bytes();
        let claim = crate::receipts::ServiceClaim::sign(
            responder,
            digest,
            sender,
            NOW_MS / 3_600_000,
            [0u8; 32],
            8,
            400,
            4_000,
        );
        let custody = crate::receipts::CustodyAttestation::sign(
            &claim,
            custodians,
            sign_custody_with.unwrap_or(responder),
        );
        let receipt = crate::receipts::ServiceReceipt::countersign(claim, sender_sk);
        Extrinsic::sign(
            sender_sk,
            PALLET_CONTRIBUTIONS,
            &ContributionsCall::SubmitReceiptWithCustody {
                receipt: Box::new(receipt),
                custody,
            },
            sender,
            nonce,
        )
    }

    /// A chain under rules v2 (this build implements them).
    fn v2(mut s: ChainState) -> ChainState {
        s.upgrade.active_version = crate::pallets::upgrade::RULES_V2;
        s
    }

    fn sized_receipt_ext(
        responder: &SigningKey,
        sender_sk: &SigningKey,
        params_b: u16,
        tokens: u32,
    ) -> Extrinsic {
        let sender = sender_sk.verifying_key().to_bytes();
        let claim = crate::receipts::ServiceClaim::sign(
            responder,
            [3u8; 32],
            sender,
            NOW_MS / 3_600_000,
            [0u8; 32],
            params_b,
            tokens,
            4_000,
        );
        // sign() clamps to the caps; a forged claim would not.
        let mut claim = claim;
        claim.model_params_b = params_b;
        claim.output_tokens = tokens;
        claim.responder_sig =
            ed25519_dalek::Signer::sign(responder, &claim.claim_digest()).to_bytes();
        let receipt = crate::receipts::ServiceReceipt::countersign(claim, sender_sk);
        Extrinsic::sign(
            sender_sk,
            PALLET_CONTRIBUTIONS,
            &ContributionsCall::SubmitReceipt {
                receipt: Box::new(receipt),
            },
            sender,
            1,
        )
    }

    #[test]
    fn v2_refuses_a_receipt_between_nodes_of_one_owner() {
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, resp) = fresh_signer();
        let (sk_sender, sender) = fresh_signer();
        let (_, wallet) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        s.identity.wallet_of.insert(resp, wallet);
        s.identity.wallet_of.insert(sender, wallet);
        let ext = receipt_ext(&sk_resp, &sk_sender, [5u8; 32], 1);

        let mut before = s.clone();
        before.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).unwrap();
        assert_eq!(before.contributions.wins_of(&resp), 1, "v1 paid it");

        let mut after = v2(s);
        assert!(after.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).is_err());
        assert_eq!(after.contributions.wins_of(&resp), 0);
    }

    #[test]
    fn v2_counts_the_hourly_pair_cap_per_owner() {
        use crate::pallets::contributions::MAX_RECEIPTS_PER_PAIR_PER_HOUR;
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, _) = fresh_signer();
        let (sk_a, a) = fresh_signer();
        let (sk_b, b) = fresh_signer();
        let (_, wallet) = fresh_signer();
        let mut s = v2(ChainState::with_genesis_authority(auth_pid));
        s.identity.wallet_of.insert(a, wallet);
        s.identity.wallet_of.insert(b, wallet);
        for n in 0..MAX_RECEIPTS_PER_PAIR_PER_HOUR {
            let mut digest = [0u8; 32];
            digest[..4].copy_from_slice(&n.to_le_bytes());
            let ext = receipt_ext(&sk_resp, &sk_a, digest, n as u64 + 1);
            s.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).unwrap();
        }
        // The wallet's other node draws on the same allowance.
        let ext = receipt_ext(&sk_resp, &sk_b, [0xee; 32], 1);
        assert!(s.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).is_err());
    }

    #[test]
    fn the_declared_model_size_and_answer_length_stay_capped_under_v2() {
        use crate::receipts::{MAX_RECEIPT_MODEL_B, MAX_RECEIPT_TOKENS};
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, _) = fresh_signer();
        let (sk_sender, _) = fresh_signer();
        let base = ChainState::with_genesis_authority(auth_pid);
        for chain in [base.clone(), v2(base.clone())] {
            let huge = sized_receipt_ext(&sk_resp, &sk_sender, 405, 400);
            assert!(chain
                .clone()
                .apply_extrinsic(&huge, 1, NOW_MS, &auth_pid)
                .is_err());
            let long = sized_receipt_ext(&sk_resp, &sk_sender, 8, MAX_RECEIPT_TOKENS + 1);
            assert!(chain
                .clone()
                .apply_extrinsic(&long, 1, NOW_MS, &auth_pid)
                .is_err());
            let fine = sized_receipt_ext(&sk_resp, &sk_sender, MAX_RECEIPT_MODEL_B, 400);
            assert!(chain
                .clone()
                .apply_extrinsic(&fine, 1, NOW_MS, &auth_pid)
                .is_ok());
        }
    }

    #[test]
    fn v2_custody_pays_neither_the_parties_own_nodes_nor_a_crowd_of_fresh_keys() {
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, _) = fresh_signer();
        let (sk_sender, sender) = fresh_signer();
        let (_, wallet) = fresh_signer();
        let (_, own) = fresh_signer();
        let (_, known) = fresh_signer();
        let fresh: Vec<PeerId> = (0..5).map(|_| fresh_signer().1).collect();
        let mut s = v2(ChainState::with_genesis_authority(auth_pid));
        s.identity.wallet_of.insert(sender, wallet);
        s.identity.wallet_of.insert(own, wallet);
        s.contributions.first_seen_ms.insert(known, NOW_MS - 1);
        let mut list = vec![own, known];
        list.extend(fresh.iter().copied());
        let ext = custody_receipt_ext(&sk_resp, &sk_sender, [9u8; 32], list, None, 1);
        s.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).unwrap();
        assert_eq!(s.contributions.custody_of(&own), 0, "the asker's own node");
        assert_eq!(s.contributions.custody_of(&known), 1);
        let paid_fresh = fresh
            .iter()
            .filter(|p| s.contributions.custody_of(p) > 0)
            .count();
        assert_eq!(
            paid_fresh, 1,
            "as many fresh custodians as established ones"
        );
    }

    #[test]
    fn v2_refuses_asks_paid_to_a_private_address() {
        assert!(crate::pallets::market::is_private_ltc_address(
            "ltcmweb1qq0abc"
        ));
        assert!(crate::pallets::market::is_private_ltc_address(
            " LTCMWEB1QQ0ABC "
        ));
        assert!(!crate::pallets::market::is_private_ltc_address(
            "ltc1qexample"
        ));
    }

    #[test]
    fn custodians_of_an_answered_question_earn_a_share_per_two_fragments() {
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, resp) = fresh_signer();
        let (sk_sender, sender) = fresh_signer();
        let (_, keeper) = fresh_signer();
        let (_, other) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        let root_before = s.state_root();

        // The asker, the responder and a repeat are not credited.
        let list = vec![keeper, other, keeper, sender, resp];
        let first = custody_receipt_ext(&sk_resp, &sk_sender, [1u8; 32], list.clone(), None, 1);
        s.apply_extrinsic(&first, 1, NOW_MS, &auth_pid).unwrap();
        assert_eq!(s.contributions.wins_of(&resp), 1, "the answer still counts");
        assert_eq!(s.contributions.custody_of(&keeper), 1);
        assert_eq!(s.contributions.custody_of(&other), 1);
        assert_eq!(s.contributions.custody_of(&sender), 0);
        assert_eq!(s.contributions.custody_of(&resp), 0);
        assert_eq!(s.shares_of(&keeper), 0, "one fragment is half a share");
        assert_ne!(s.state_root(), root_before);

        let second = custody_receipt_ext(&sk_resp, &sk_sender, [2u8; 32], list, None, 2);
        s.apply_extrinsic(&second, 2, NOW_MS, &auth_pid).unwrap();
        assert_eq!(s.contributions.custody_of(&keeper), 2);
        assert_eq!(s.shares_of(&keeper), 1, "two fragments make one share");
        assert_eq!(s.contributions.redeemable(&keeper).0, 1);
    }

    #[test]
    fn a_custody_list_the_responder_did_not_sign_is_refused_whole() {
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, resp) = fresh_signer();
        let (sk_sender, _) = fresh_signer();
        let (_, keeper) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        // Signed by the asker instead of the responder: an asker cannot
        // credit custodians on its own.
        let forged = custody_receipt_ext(
            &sk_resp,
            &sk_sender,
            [3u8; 32],
            vec![keeper],
            Some(&sk_sender),
            1,
        );
        assert!(s.apply_extrinsic(&forged, 1, NOW_MS, &auth_pid).is_err());
        assert_eq!(s.contributions.custody_of(&keeper), 0);
        assert_eq!(
            s.contributions.wins_of(&resp),
            0,
            "nothing of the call applies"
        );
    }

    #[test]
    fn a_receipt_submitted_by_someone_other_than_the_sender_is_refused() {
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, _) = fresh_signer();
        let (sk_sender, _) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        let mut ext = receipt_ext(&sk_resp, &sk_sender, [6u8; 32], 1);
        let resp_pid = sk_resp.verifying_key().to_bytes();
        ext = Extrinsic::sign(
            &sk_resp,
            PALLET_CONTRIBUTIONS,
            &DecodedCall::decode(ext.pallet_idx, &ext.args)
                .ok()
                .and_then(|d| match d {
                    DecodedCall::Contributions(c) => Some(c),
                    _ => None,
                })
                .unwrap(),
            resp_pid,
            1,
        );
        assert!(s.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).is_err());
    }

    #[test]
    fn redeem_mints_are_counted_in_total_minted() {
        let (_, auth_pid) = fresh_signer();
        let (sk_peer, peer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        s.contributions
            .lottery_dimension_by_peer
            .insert(peer, 5_000);
        let before = s.nai.total_minted;
        let ext = Extrinsic::sign(
            &sk_peer,
            PALLET_CONTRIBUTIONS,
            &ContributionsCall::Redeem {
                receiver: peer,
                shares: 0,
                lottery_dimension: 5_000,
            },
            peer,
            1,
        );
        s.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).unwrap();
        let minted = s.balance_of(&peer);
        assert!(minted > 0);
        assert_eq!(s.nai.total_minted, before + minted);
    }

    #[test]
    fn a_seller_cannot_raise_a_claim_for_a_buyer_without_standing() {
        use crate::pallets::market::MarketCall;
        let (_, auth_pid) = fresh_signer();
        let (sk_seller, seller) = fresh_signer();
        let (_, newcomer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        seed_ask(&mut s, &sk_seller, seller);

        let claim = market_ext(
            &sk_seller,
            seller,
            MarketCall::ClaimFill {
                ask_id: 0,
                buyer: newcomer,
                payment_ref: "ltc-tx-for-newcomer".to_string(),
            },
            2,
        );
        assert!(s.apply_extrinsic(&claim, 2, NOW_MS, &auth_pid).is_err());
        assert!(s.market.claims.is_empty());
        assert_eq!(s.market.escrowed, 400, "the escrow is untouched");
    }

    #[test]
    fn anyone_with_standing_can_post_an_ask_without_being_an_authority() {
        let (_, auth_pid) = fresh_signer();
        let (sk_seller, seller) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        seed_ask(&mut s, &sk_seller, seller);
        assert_eq!(s.balance_of(&seller), 600);
    }

    #[test]
    fn an_account_without_standing_cannot_post_an_ask() {
        use crate::pallets::market::MarketCall;
        let (_, auth_pid) = fresh_signer();
        let (sk_new, newcomer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        s.balances.mint(newcomer, 1_000);

        let post = market_ext(
            &sk_new,
            newcomer,
            MarketCall::PostAsk {
                credits: 400,
                ask_sats: 8_000_000,
                ltc_address: "ltc1qnewcomer".to_string(),
            },
            1,
        );
        assert!(s.apply_extrinsic(&post, 1, NOW_MS, &auth_pid).is_err());
        assert!(s.market.asks.is_empty());
        assert_eq!(
            s.balance_of(&newcomer),
            1_000,
            "a refused ask escrows nothing"
        );
    }

    #[test]
    fn settling_a_listing_requires_the_sale_oracle() {
        let (_, auth_pid) = fresh_signer();
        let (sk_seller, seller) = fresh_signer();
        let (sk_rando, rando) = fresh_signer();
        let (sk_buyer, buyer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        seed_ask(&mut s, &sk_seller, seller);
        seed_claim(&mut s, &sk_buyer, buyer, &auth_pid);

        let err = s
            .apply_extrinsic(
                &settle_ext(&sk_rando, rando, buyer, 1),
                3,
                NOW_MS,
                &auth_pid,
            )
            .unwrap_err();
        assert!(matches!(err, ChainError::UnauthorizedAuthorityCall));
        assert_eq!(s.balance_of(&buyer), 0);
        assert_eq!(
            s.market.escrowed, 400,
            "escrow must survive a refused settle"
        );
    }

    #[test]
    fn the_sale_oracle_can_settle_a_claimed_listing() {
        let (_, auth_pid) = fresh_signer();
        let (sk_seller, seller) = fresh_signer();
        let (sk_oracle, oracle) = fresh_signer();
        let (sk_buyer, buyer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        seed_ask(&mut s, &sk_seller, seller);
        s.nai.sale_oracle = Some(oracle);
        seed_claim(&mut s, &sk_buyer, buyer, &auth_pid);

        s.apply_extrinsic(
            &settle_ext(&sk_oracle, oracle, buyer, 1),
            3,
            NOW_MS,
            &auth_pid,
        )
        .unwrap();

        assert_eq!(s.balance_of(&buyer), 400);
        assert_eq!(s.balance_of(&seller), 600, "the seller is paid off chain");
        assert_eq!(s.market.escrowed, 0);
    }

    #[test]
    fn the_sale_oracle_cannot_settle_an_unclaimed_listing() {
        let (_, auth_pid) = fresh_signer();
        let (sk_seller, seller) = fresh_signer();
        let (sk_oracle, oracle) = fresh_signer();
        let (_, buyer) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        seed_ask(&mut s, &sk_seller, seller);
        s.nai.sale_oracle = Some(oracle);

        let err = s
            .apply_extrinsic(
                &settle_ext(&sk_oracle, oracle, buyer, 1),
                3,
                NOW_MS,
                &auth_pid,
            )
            .unwrap_err();
        assert!(matches!(err, ChainError::Pallet(_)));
        assert_eq!(s.market.escrowed, 400);
    }

    #[test]
    fn a_posted_listing_is_reflected_in_the_state_root() {
        let (_, auth_pid) = fresh_signer();
        let (sk_seller, seller) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        let before = s.state_root();
        seed_ask(&mut s, &sk_seller, seller);
        assert_ne!(before, s.state_root());
    }

    fn producer_key() -> SigningKey {
        SigningKey::from_bytes(&[0x42u8; 32])
    }

    fn test_chain() -> ChainState {
        ChainState::with_genesis_authority(producer_key().verifying_key().to_bytes())
    }

    fn seal_with(state: &ChainState, mut block: Block, key: &SigningKey) -> Block {
        block.header.author = crate::validators::authority_peer_id(&key.verifying_key());
        block.header.extrinsics_root = block.extrinsics_root();
        block.header.lottery_root = block.lottery_root();
        let mut probe = state.clone();
        if probe.apply_block_unverified(block.clone()).is_ok() {
            block.header.state_root = probe.state_root();
        }
        block.header.sign(key);
        block
    }

    fn finalize(state: &ChainState, block: Block) -> Block {
        seal_with(state, block, &producer_key())
    }

    #[test]
    fn a_body_swapped_under_a_signed_header_is_rejected() {
        let (sk_alice, alice) = fresh_signer();
        let mut s = test_chain();
        s.balances.mint(alice, 100);
        let honest = finalize(&s, empty_block_on_top_of(&s, 1, 1));
        let mut forged = honest.clone();
        forged.body.extrinsics.push(Extrinsic::sign(
            &sk_alice,
            PALLET_BALANCES,
            &BalancesCall::Transfer {
                to: [9u8; 32],
                amount: 10,
            },
            alice,
            1,
        ));
        assert_eq!(forged.header.block_hash(), honest.header.block_hash());
        assert_eq!(s.apply_block(forged), Err(ChainError::BadExtrinsicsRoot));
        assert_eq!(s.balance_of(&alice), 100);
    }

    #[test]
    fn a_wrong_state_root_is_rejected_and_rolled_back() {
        let (sk_alice, alice) = fresh_signer();
        let mut s = test_chain();
        s.balances.mint(alice, 100);
        let mut block = empty_block_on_top_of(&s, 1, 1);
        block.body.extrinsics.push(Extrinsic::sign(
            &sk_alice,
            PALLET_BALANCES,
            &BalancesCall::Transfer {
                to: [9u8; 32],
                amount: 10,
            },
            alice,
            1,
        ));
        let mut block = finalize(&s, block);
        block.header.state_root = [0xAB; 32];
        block.header.sign(&producer_key());
        assert_eq!(s.apply_block(block), Err(ChainError::BadStateRoot));
        assert_eq!(s.balance_of(&alice), 100);
        assert_eq!(s.block_count(), 0);
    }

    #[test]
    fn a_failed_block_restores_the_market_escrow() {
        let (sk_auth, auth_pid) = fresh_signer();
        let (sk_seller, seller) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        grant_standing(&mut s, seller);
        s.balances.mint(seller, 1_000);
        let post = market_ext(
            &sk_seller,
            seller,
            crate::pallets::market::MarketCall::PostAsk {
                credits: 1_000,
                ask_sats: 10,
                ltc_address: "ltc1qexample".to_string(),
            },
            1,
        );
        let bad = Extrinsic::sign(
            &sk_seller,
            PALLET_BALANCES,
            &BalancesCall::Transfer {
                to: [9u8; 32],
                amount: 5_000,
            },
            seller,
            2,
        );
        let mut block = empty_block_on_top_of(&s, 1, NOW_MS / crate::block::SLOT_DURATION_MS);
        block.body.extrinsics = vec![post, bad];
        let block = seal_with(&s, block, &sk_auth);
        assert_eq!(
            s.apply_block(block),
            Err(ChainError::BlockContainsInvalidExtrinsic)
        );
        assert!(s.market.asks.is_empty());
        assert_eq!(s.market.escrowed, 0);
        assert_eq!(s.balance_of(&seller), 1_000);
    }

    fn empty_block_on_top_of(state: &ChainState, height: u64, slot: u64) -> Block {
        Block {
            header: BlockHeader {
                version: 1,
                height,
                slot,
                parent_hash: state.head_hash(),
                state_root: [0u8; 32],
                extrinsics_root: [0u8; 32],
                lottery_root: [0u8; 32],
                author: [0u8; 32],
                signature: [0u8; 64],
                timestamp_ms: slot * crate::block::SLOT_DURATION_MS,
            },
            body: BlockBody {
                extrinsics: Vec::new(),
                lottery_results: Vec::new(),
            },
        }
    }

    #[test]
    fn empty_chain_head_is_genesis() {
        let s = ChainState::new();
        assert_eq!(s.head_height(), 0);
        assert_eq!(
            s.head_hash(),
            ChainState::genesis_block().header.block_hash()
        );
        assert_eq!(s.block_count(), 0);
    }

    #[test]
    fn treasury_genesis_mint_lands_in_authority_wallet() {
        let (_, authority_pid) = fresh_signer();
        let s = ChainState::with_genesis_authority(authority_pid);
        assert_eq!(s.balance_of(&authority_pid), TREASURY_GENESIS_AMOUNT);
        assert_eq!(s.nai.total_minted, TREASURY_GENESIS_AMOUNT);
        let other: PeerId = [9u8; 32];
        assert_eq!(s.balance_of(&other), 0);
    }

    #[test]
    fn treasury_genesis_skipped_with_zero_pubkey() {
        let s = ChainState::with_genesis_authority([0u8; 32]);
        assert_eq!(s.nai.total_minted, 0);
    }

    #[test]
    fn treasury_authority_can_transfer_from_genesis_pool() {
        use crate::pallets::balances::BalancesCall;
        let (sk_authority, authority_pid) = fresh_signer();
        let receiver: PeerId = [7u8; 32];

        let mut s = ChainState::with_genesis_authority(authority_pid);
        assert_eq!(s.balance_of(&authority_pid), TREASURY_GENESIS_AMOUNT);

        let call = BalancesCall::Transfer {
            to: receiver,
            amount: 100_000,
        };
        let ext = Extrinsic::sign(&sk_authority, PALLET_BALANCES, &call, authority_pid, 1);
        s.apply_extrinsic(&ext, 1, 0, &authority_pid).unwrap();

        assert_eq!(
            s.balance_of(&authority_pid),
            TREASURY_GENESIS_AMOUNT - 100_000
        );
        assert_eq!(s.balance_of(&receiver), 100_000);
    }

    #[test]
    fn apply_empty_block_advances_head() {
        let mut s = test_chain();
        let block = finalize(&s, empty_block_on_top_of(&s, 1, 1));
        let expected_hash = block.header.block_hash();
        s.apply_block(block).expect("apply_block");
        assert_eq!(s.head_height(), 1);
        assert_eq!(s.head_hash(), expected_hash);
        assert_eq!(s.system.block_height, 1);
    }

    #[test]
    fn wrong_height_rejected() {
        let mut s = test_chain();
        let mut block = empty_block_on_top_of(&s, 1, 1);
        block.header.height = 5;
        let result = s.apply_block(finalize(&s, block));
        assert_eq!(result, Err(ChainError::BadBlockHeight));
        assert_eq!(s.block_count(), 0);
    }

    #[test]
    fn transfer_extrinsic_inside_block_moves_balance() {
        let (sk_alice, alice) = fresh_signer();
        let bob: PeerId = [9u8; 32];

        let mut s = test_chain();
        s.balances.mint(alice, 100);

        let call = BalancesCall::Transfer {
            to: bob,
            amount: 30,
        };
        let ext = Extrinsic::sign(&sk_alice, PALLET_BALANCES, &call, alice, 1);

        let mut block = empty_block_on_top_of(&s, 1, 1);
        block.body.extrinsics.push(ext);

        s.apply_block(finalize(&s, block))
            .expect("block with transfer");
        assert_eq!(s.balance_of(&alice), 70);
        assert_eq!(s.balance_of(&bob), 30);
        assert_eq!(s.system.nonces[&alice], 1);
    }

    #[test]
    fn invalid_extrinsic_rolls_back_block() {
        let (sk_alice, alice) = fresh_signer();
        let bob: PeerId = [9u8; 32];

        let mut s = test_chain();
        s.balances.mint(alice, 10);

        let call = BalancesCall::Transfer {
            to: bob,
            amount: 50,
        };
        let ext = Extrinsic::sign(&sk_alice, PALLET_BALANCES, &call, alice, 1);

        let mut block = empty_block_on_top_of(&s, 1, 1);
        block.body.extrinsics.push(ext);

        let result = s.apply_block(finalize(&s, block));
        assert_eq!(result, Err(ChainError::BlockContainsInvalidExtrinsic));
        assert_eq!(s.balance_of(&alice), 10);
        assert_eq!(s.balance_of(&bob), 0);
        assert!(!s.system.nonces.contains_key(&alice));
        assert_eq!(s.block_count(), 0);
    }

    #[test]
    fn lottery_result_in_block_bumps_wins() {
        let mut s = test_chain();
        let winner: PeerId = [7u8; 32];
        let mut block = empty_block_on_top_of(&s, 1, 1);
        let model_b = 4u64;
        let tokens = 25u64;
        let gen_ms = 5_000u64;
        let dim = crate::lottery::compute_dimension(model_b, tokens, gen_ms);
        block.body.lottery_results.push(LotteryResult {
            hour_epoch: 1,
            msg_len: 100,
            n_engines: 1,
            room_salt: [0u8; 32],
            winner,
            dimension: dim,
            responder_model_b: model_b as u16,
            responder_tokens: tokens as u32,
            responder_gen_ms: gen_ms as u32,
        });

        s.apply_block(finalize(&s, block)).unwrap();
        assert_eq!(s.contributions.wins_of(&winner), 1);
        assert_eq!(s.contributions.redeemable_lottery_dimension(&winner), dim);
    }

    #[test]
    fn state_root_deterministic_under_reorder() {
        let mut a = ChainState::new();
        let mut b = ChainState::new();
        a.balances.mint([1u8; 32], 100);
        a.balances.mint([2u8; 32], 200);
        b.balances.mint([2u8; 32], 200);
        b.balances.mint([1u8; 32], 100);
        assert_eq!(a.state_root(), b.state_root());
    }

    #[test]
    fn nonce_replay_rejected_by_dispatcher() {
        let (sk_alice, alice) = fresh_signer();
        let bob: PeerId = [9u8; 32];

        let mut s = ChainState::new();
        s.balances.mint(alice, 100);

        let call = BalancesCall::Transfer {
            to: bob,
            amount: 10,
        };
        let ext = Extrinsic::sign(&sk_alice, PALLET_BALANCES, &call, alice, 1);

        s.apply_extrinsic(&ext, 1, 0, &alice).unwrap();
        let replay = s.apply_extrinsic(&ext, 1, 0, &alice);
        assert_eq!(replay, Err(ChainError::Pallet(PalletError::NonceReplay)));
    }

    #[test]
    fn authority_add_via_dispatcher_promotes_new_validator() {
        use crate::extrinsic::PALLET_AUTHORITY;
        use crate::pallets::authority::AuthorityCall;

        let (sk_genesis, genesis_pid) = fresh_signer();
        let (_, new_pubkey) = fresh_signer();

        let mut s = ChainState::with_genesis_authority(genesis_pid);
        assert_eq!(s.authority.len(), 1);

        let call = AuthorityCall::Add {
            pubkey: new_pubkey,
            channels: vec!["large_chat".to_string()],
            attestation_hash: [42u8; 32],
        };
        let ext = Extrinsic::sign(&sk_genesis, PALLET_AUTHORITY, &call, genesis_pid, 1);
        s.apply_extrinsic(&ext, 1, 0, &genesis_pid).unwrap();

        assert_eq!(s.authority.len(), 2);
        assert!(s.authority.is_authority_signer(&new_pubkey));
        assert_eq!(
            s.authority.metadata[&new_pubkey].channels,
            vec!["large_chat".to_string()]
        );
    }

    #[test]
    fn authority_add_from_non_authority_rejected_by_dispatcher() {
        use crate::extrinsic::PALLET_AUTHORITY;
        use crate::pallets::authority::AuthorityCall;

        let (_, genesis_pid) = fresh_signer();
        let (sk_eve, eve_pid) = fresh_signer();
        let (_, target_pubkey) = fresh_signer();

        let mut s = ChainState::with_genesis_authority(genesis_pid);

        let call = AuthorityCall::Add {
            pubkey: target_pubkey,
            channels: vec![],
            attestation_hash: [0u8; 32],
        };
        let ext = Extrinsic::sign(&sk_eve, PALLET_AUTHORITY, &call, eve_pid, 1);
        let result = s.apply_extrinsic(&ext, 1, 0, &genesis_pid);
        assert_eq!(result, Err(ChainError::UnauthorizedAuthorityCall));
        assert_eq!(s.authority.len(), 1, "validator set unchanged");
    }

    #[test]
    fn block_with_dimension_lie_is_rejected() {
        // Authority cannot inflate dimension above what
        // compute_dimension(model, tokens, ms) yields. The block
        // is rolled back entirely.
        let mut s = test_chain();
        let mut block = empty_block_on_top_of(&s, 1, 1);
        let model_b = 4u64;
        let tokens = 25u64;
        let gen_ms = 5_000u64;
        let honest = crate::lottery::compute_dimension(model_b, tokens, gen_ms);
        block.body.lottery_results.push(LotteryResult {
            hour_epoch: 1,
            msg_len: 0,
            n_engines: 1,
            room_salt: [0u8; 32],
            winner: [7u8; 32],
            // Lie: claim 4B inference earns 405B-class dimension.
            dimension: honest.saturating_mul(100),
            responder_model_b: model_b as u16,
            responder_tokens: tokens as u32,
            responder_gen_ms: gen_ms as u32,
        });
        let result = s.apply_block(finalize(&s, block));
        assert_eq!(result, Err(ChainError::LotteryDimensionMismatch));
        // Rollback: winner has no recorded dimension.
        assert_eq!(s.contributions.wins_of(&[7u8; 32]), 0);
        assert_eq!(s.contributions.redeemable_lottery_dimension(&[7u8; 32]), 0);
    }

    #[test]
    fn economy_record_block_activity_tracks_lottery_dimension() {
        let mut s = test_chain();
        let mut block = empty_block_on_top_of(&s, 1, 1);
        let model_b = 32u64;
        let tokens = 100u64;
        let gen_ms = 1_000u64;
        let dim = crate::lottery::compute_dimension(model_b, tokens, gen_ms);
        block.body.lottery_results.push(LotteryResult {
            hour_epoch: 1,
            msg_len: 0,
            n_engines: 1,
            room_salt: [0u8; 32],
            winner: [7u8; 32],
            dimension: dim,
            responder_model_b: model_b as u16,
            responder_tokens: tokens as u32,
            responder_gen_ms: gen_ms as u32,
        });
        s.apply_block(finalize(&s, block)).unwrap();
        assert_eq!(s.economy.recent_activity.len(), 1);
        assert_eq!(s.economy.recent_activity[0].1, dim);
    }

    #[test]
    fn redeem_uses_dynamic_shares_per_credit_from_economy() {
        use crate::pallets::contributions::ContributionsCall;
        let (sk, pid) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(pid);
        s.economy.current_shares_per_credit = 10; // 1 credit per 10 share-equivs

        for _ in 0..50 {
            *s.contributions.share_counts.entry(pid).or_insert(0) += 1;
        }

        let call = ContributionsCall::Redeem {
            receiver: pid,
            shares: 50,
            lottery_dimension: 0,
        };
        let ext = Extrinsic::sign(&sk, PALLET_CONTRIBUTIONS, &call, pid, 1);
        s.apply_extrinsic(&ext, 1, 0, &pid).unwrap();
        assert_eq!(s.balance_of(&pid), TREASURY_GENESIS_AMOUNT + 5);
    }

    #[test]
    fn record_share_from_non_authority_rejected_when_validators_present() {
        let (sk_authority, authority_pid) = fresh_signer();
        let (sk_eve, eve_pid) = fresh_signer();

        let mut s = ChainState::with_genesis_authority(authority_pid);

        let call = ContributionsCall::RecordShare { peer: [3u8; 32] };
        let ext = Extrinsic::sign(&sk_authority, PALLET_CONTRIBUTIONS, &call, authority_pid, 1);
        s.apply_extrinsic(&ext, 1, 0, &authority_pid).unwrap();

        let eve_ext = Extrinsic::sign(&sk_eve, PALLET_CONTRIBUTIONS, &call, eve_pid, 1);
        let result = s.apply_extrinsic(&eve_ext, 1, 0, &authority_pid);
        assert_eq!(result, Err(ChainError::UnauthorizedAuthorityCall));
    }

    fn authority_ext(
        sk: &SigningKey,
        signer: PeerId,
        call: AuthorityCall,
        nonce: u64,
    ) -> Extrinsic {
        Extrinsic::sign(sk, crate::extrinsic::PALLET_AUTHORITY, &call, signer, nonce)
    }

    fn gov_ext(
        sk: &SigningKey,
        signer: PeerId,
        call: crate::pallets::governance::GovernanceCall,
        nonce: u64,
    ) -> Extrinsic {
        Extrinsic::sign(
            sk,
            crate::extrinsic::PALLET_GOVERNANCE,
            &call,
            signer,
            nonce,
        )
    }

    fn add_partner(
        s: &mut ChainState,
        founder_sk: &SigningKey,
        founder: PeerId,
        nonce: u64,
    ) -> (SigningKey, PeerId) {
        let (sk, pid) = fresh_signer();
        let ext = authority_ext(
            founder_sk,
            founder,
            AuthorityCall::Add {
                pubkey: pid,
                channels: vec!["chat".into()],
                attestation_hash: [0u8; 32],
            },
            nonce,
        );
        s.apply_extrinsic(&ext, 1, NOW_MS, &founder).unwrap();
        (sk, pid)
    }

    fn give_vote(s: &mut ChainState, peer: PeerId, weight: u64) {
        s.contributions.share_counts.insert(peer, weight);
        s.contributions.first_seen_ms.insert(peer, 0);
    }

    fn pass_proposal(
        s: &mut ChainState,
        voters: &[(SigningKey, PeerId)],
        action: GovAction,
        start: u64,
    ) -> u64 {
        use crate::pallets::governance::{
            GovernanceCall, ENACTMENT_DELAY_BLOCKS, VOTING_PERIOD_BLOCKS,
        };
        let id = s.governance.next_id;
        let (psk, ppid) = &voters[0];
        let nonce = |s: &ChainState, p: &PeerId| s.system.nonces.get(p).copied().unwrap_or(0) + 1;
        let ext = gov_ext(
            psk,
            *ppid,
            GovernanceCall::Propose { action },
            nonce(s, ppid),
        );
        s.apply_extrinsic(&ext, start, NOW_MS, ppid).unwrap();
        for (sk, pid) in voters {
            let ext = gov_ext(
                sk,
                *pid,
                GovernanceCall::Vote { id, aye: true },
                nonce(s, pid),
            );
            s.apply_extrinsic(&ext, start + 1, NOW_MS, pid).unwrap();
        }
        let at = start + VOTING_PERIOD_BLOCKS + ENACTMENT_DELAY_BLOCKS;
        let ext = gov_ext(psk, *ppid, GovernanceCall::Enact { id }, nonce(s, ppid));
        s.apply_extrinsic(&ext, at, NOW_MS, ppid).unwrap();
        at
    }

    #[test]
    fn a_partner_cannot_add_remove_others_or_dissolve() {
        let (fsk, founder) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        let (psk, partner) = add_partner(&mut s, &fsk, founder, 1);
        let (_, partner2) = add_partner(&mut s, &fsk, founder, 2);
        let (_, stranger) = fresh_signer();

        let add = authority_ext(
            &psk,
            partner,
            AuthorityCall::Add {
                pubkey: stranger,
                channels: vec![],
                attestation_hash: [0u8; 32],
            },
            1,
        );
        assert_eq!(
            s.apply_extrinsic(&add, 2, NOW_MS, &founder),
            Err(ChainError::UnauthorizedAuthorityCall)
        );
        let remove_other =
            authority_ext(&psk, partner, AuthorityCall::Remove { pubkey: partner2 }, 1);
        assert!(s
            .apply_extrinsic(&remove_other, 2, NOW_MS, &founder)
            .is_err());
        let remove_founder =
            authority_ext(&psk, partner, AuthorityCall::Remove { pubkey: founder }, 2);
        assert!(s
            .apply_extrinsic(&remove_founder, 2, NOW_MS, &founder)
            .is_err());
        let dissolve = authority_ext(&psk, partner, AuthorityCall::Dissolve, 10);
        assert!(s.apply_extrinsic(&dissolve, 2, NOW_MS, &founder).is_err());
        let oracle = Extrinsic::sign(
            &psk,
            crate::extrinsic::PALLET_NAI,
            &NaiCall::SetSaleOracle {
                pubkey: Some(partner),
            },
            partner,
            11,
        );
        assert!(s.apply_extrinsic(&oracle, 2, NOW_MS, &founder).is_err());
        assert_eq!(s.authority.len(), 3);

        let resign = authority_ext(&psk, partner, AuthorityCall::Remove { pubkey: partner }, 3);
        s.apply_extrinsic(&resign, 2, NOW_MS, &founder).unwrap();
        assert!(!s.authority.is_authority_signer(&partner));

        let founder_removes =
            authority_ext(&fsk, founder, AuthorityCall::Remove { pubkey: partner2 }, 3);
        s.apply_extrinsic(&founder_removes, 3, NOW_MS, &founder)
            .unwrap();
        assert_eq!(s.authority.authorities, vec![founder]);
    }

    #[test]
    fn dissolution_waits_for_governance_to_prove_it_can_replace_the_oracle() {
        let (fsk, founder) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        let dissolve = authority_ext(&fsk, founder, AuthorityCall::Dissolve, 1);
        assert_eq!(
            s.apply_extrinsic(&dissolve, 1, NOW_MS, &founder),
            Err(ChainError::GovernanceNotProven)
        );

        let voters: Vec<(SigningKey, PeerId)> = (0..3).map(|_| fresh_signer()).collect();
        for (_, p) in &voters {
            give_vote(&mut s, *p, 100);
        }
        let (_, new_oracle) = fresh_signer();
        let at = pass_proposal(
            &mut s,
            &voters,
            GovAction::SetSaleOracle {
                pubkey: Some(new_oracle),
            },
            10,
        );
        assert_eq!(s.nai.sale_oracle, Some(new_oracle));
        assert!(s.governance.oracle_replaced_by_vote);

        let dissolve = authority_ext(&fsk, founder, AuthorityCall::Dissolve, 1);
        s.apply_extrinsic(&dissolve, at + 1, NOW_MS, &founder)
            .unwrap();
        assert!(s.authority.is_dissolved());
        assert_eq!(s.authority.founder, None);

        let (_, someone) = fresh_signer();
        let add = authority_ext(
            &fsk,
            founder,
            AuthorityCall::Add {
                pubkey: someone,
                channels: vec![],
                attestation_hash: [0u8; 32],
            },
            2,
        );
        assert!(s.apply_extrinsic(&add, at + 2, NOW_MS, &founder).is_err());
        let emission = Extrinsic::sign(
            &fsk,
            crate::extrinsic::PALLET_ECONOMY,
            &crate::pallets::economy::EconomyCall::SetEmissionTarget {
                target_credits_per_hour: 1,
            },
            founder,
            2,
        );
        assert!(
            s.apply_extrinsic(&emission, at + 2, NOW_MS, &founder)
                .is_err(),
            "after dissolution the economy moves only by vote"
        );

        let before = s.economy.target_credits_per_hour;
        pass_proposal(
            &mut s,
            &voters,
            GovAction::SetEmissionTarget {
                target_credits_per_hour: before + 7,
            },
            at + 10,
        );
        assert_eq!(s.economy.target_credits_per_hour, before + 7);
    }

    #[test]
    fn nobody_can_remove_the_founder_or_touch_membership_while_it_is_active() {
        let (fsk, founder) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        let (_, partner) = add_partner(&mut s, &fsk, founder, 1);
        let (vsk, voter) = fresh_signer();
        give_vote(&mut s, voter, 100);
        for action in [
            GovAction::RemovePartner { pubkey: founder },
            GovAction::RemovePartner { pubkey: partner },
        ] {
            let ext = gov_ext(
                &vsk,
                voter,
                crate::pallets::governance::GovernanceCall::Propose { action },
                s.system.nonces.get(&voter).copied().unwrap_or(0) + 1,
            );
            assert!(s.apply_extrinsic(&ext, 2, NOW_MS, &founder).is_err());
        }
        assert!(s.authority.is_founder(&founder));
        assert!(s.authority.is_authority_signer(&partner));
    }

    #[test]
    fn dissolution_ends_every_founder_title_but_keeps_the_nodes_working() {
        let (fsk, founder) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        let (psk, partner) = add_partner(&mut s, &fsk, founder, 1);
        let validators_before = s.validator_set().peer_ids.clone();
        let channels_before = s.authority.metadata[&partner].channels.clone();
        s.governance.oracle_replaced_by_vote = true;
        let dissolve = authority_ext(&fsk, founder, AuthorityCall::Dissolve, 2);
        s.apply_extrinsic(&dissolve, 2, NOW_MS, &founder).unwrap();

        assert!(!s.authority.is_founder(&founder));
        assert!(!s.authority.is_founder(&partner));
        assert_eq!(
            s.validator_set().peer_ids,
            validators_before,
            "block producers unchanged"
        );
        assert_eq!(
            s.authority.metadata[&partner].channels, channels_before,
            "channels unchanged"
        );
        assert!(s.authority.serves_channel(&partner, "chat"));

        let set = authority_ext(
            &psk,
            partner,
            AuthorityCall::SetChannels {
                pubkey: partner,
                channels: vec!["chat".into(), "frontier".into()],
            },
            1,
        );
        s.apply_extrinsic(&set, 3, NOW_MS, &founder).unwrap();
        assert!(s.authority.serves_channel(&partner, "frontier"));
    }

    #[test]
    fn a_failed_enactment_is_recorded_without_rejecting_the_block() {
        let (_, founder) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        let voters: Vec<(SigningKey, PeerId)> = vec![fresh_signer()];
        s.authority.dissolve(0).unwrap();
        give_vote(&mut s, voters[0].1, 10);
        pass_proposal(
            &mut s,
            &voters,
            GovAction::RemovePartner { pubkey: [42u8; 32] },
            1,
        );
        assert!(matches!(
            s.governance.proposals[&0].status,
            crate::pallets::governance::ProposalStatus::Failed { .. }
        ));
    }

    fn two_validators() -> (ChainState, SigningKey, SigningKey) {
        let founder = producer_key();
        let partner = SigningKey::from_bytes(&[0x43u8; 32]);
        let mut s = test_chain();
        let add = authority_ext(
            &founder,
            founder.verifying_key().to_bytes(),
            AuthorityCall::Add {
                pubkey: partner.verifying_key().to_bytes(),
                channels: vec!["chat".into()],
                attestation_hash: [0u8; 32],
            },
            1,
        );
        let block = empty_block_on_top_of(&s, 1, 2);
        let mut block = block;
        block.body.extrinsics.push(add);
        let block = seal_with(&s, block, &founder);
        s.apply_block(block).unwrap();
        (s, founder, partner)
    }

    fn next_block(s: &ChainState, slot: u64, key: &SigningKey) -> Block {
        seal_with(s, empty_block_on_top_of(s, s.head_height() + 1, slot), key)
    }

    #[test]
    fn partners_produce_in_their_own_slots_only() {
        let (mut s, founder, partner) = two_validators();
        assert_eq!(s.validator_set().len(), 2);
        assert_eq!(
            s.apply_block(next_block(&s, 4, &partner)),
            Err(ChainError::BadBlockSignature),
            "slot 4 belongs to the founder"
        );
        s.apply_block(next_block(&s, 5, &partner)).unwrap();
        assert_eq!(
            s.apply_block(next_block(&s, 5, &partner)),
            Err(ChainError::BadSlot)
        );
        s.apply_block(next_block(&s, 8, &founder)).unwrap();
        assert_eq!(s.head_height(), 3);
    }

    #[test]
    fn the_chain_keeps_producing_after_the_founder_dissolves() {
        let (mut s, founder, partner) = two_validators();
        s.governance.oracle_replaced_by_vote = true;
        let dissolve = authority_ext(
            &founder,
            founder.verifying_key().to_bytes(),
            AuthorityCall::Dissolve,
            2,
        );
        let mut block = empty_block_on_top_of(&s, 2, 4);
        block.body.extrinsics.push(dissolve);
        let block = seal_with(&s, block, &founder);
        s.apply_block(block).unwrap();
        assert!(s.authority.is_dissolved());
        for slot in [5u64, 7, 9] {
            s.apply_block(next_block(&s, slot, &partner)).unwrap();
        }
        assert_eq!(s.head_height(), 5);
    }

    #[test]
    fn a_longer_fork_replaces_the_head_and_returns_orphaned_extrinsics() {
        let (base, founder, partner) = two_validators();
        let founder_pid = founder.verifying_key().to_bytes();
        let mut ours = base.clone();
        let theirs_base = base.clone();

        let mut b = empty_block_on_top_of(&ours, 2, 4);
        b.body.extrinsics.push(Extrinsic::sign(
            &founder,
            PALLET_BALANCES,
            &BalancesCall::Transfer {
                to: [9u8; 32],
                amount: 5,
            },
            founder_pid,
            2,
        ));
        let b = seal_with(&ours, b, &founder);
        ours.apply_block(b).unwrap();
        assert_eq!(ours.balance_of(&[9u8; 32]), 5);

        let mut theirs = theirs_base;
        let mut fork = Vec::new();
        for slot in [5u64, 7] {
            let blk = next_block(&theirs, slot, &partner);
            theirs.apply_block(blk.clone()).unwrap();
            fork.push(blk);
        }

        let equal = ours.clone().try_reorg(fork[..1].to_vec(), 0);
        assert!(matches!(equal, Err(ChainError::NotLonger)));
        let mut switched = ours.clone();
        let orphaned = switched.try_reorg(fork, 0).unwrap();
        assert_eq!(switched.blocks.len(), 3);
        assert_eq!(switched.hash_at(1), base.hash_at(1));
        assert_eq!(switched.head_height(), 3);
        assert_eq!(switched.head_hash(), theirs.head_hash());
        assert_eq!(switched.state_root(), theirs.state_root());
        assert_eq!(orphaned.len(), 1);
        assert_eq!(switched.balance_of(&[9u8; 32]), 0);
    }

    #[test]
    fn a_fork_deeper_than_the_limit_is_refused() {
        let mut s = test_chain();
        let key = producer_key();
        let mut alt = s.clone();
        for slot in 1..=(MAX_REORG_DEPTH + 10) {
            let b = next_block(&s, slot, &key);
            s.apply_block(b).unwrap();
        }
        let mut fork = Vec::new();
        for slot in (1_000..).take((MAX_REORG_DEPTH + 20) as usize) {
            let b = next_block(&alt, slot, &key);
            alt.apply_block(b.clone()).unwrap();
            fork.push(b);
        }
        assert!(matches!(
            s.clone().try_reorg(fork, 0),
            Err(ChainError::ReorgTooDeep)
        ));

        let fork_at = s.head_height() - 10;
        let mut near = s.rewound_to(fork_at).unwrap();
        let mut blocks = Vec::new();
        for slot in (5_000..).take(12) {
            let b = next_block(&near, slot, &key);
            near.apply_block(b.clone()).unwrap();
            blocks.push(b);
        }
        let mut switched = s.clone();
        switched.try_reorg(blocks, 0).unwrap();
        assert_eq!(switched.head_hash(), near.head_hash());
    }

    #[test]
    fn the_sender_of_a_landed_receipt_can_rate_the_responder() {
        use crate::pallets::reputation::{DislikeReason, ReputationCall, Verdict};
        let (_, auth_pid) = fresh_signer();
        let (sk_resp, resp) = fresh_signer();
        let (sk_sender, sender) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(auth_pid);
        let ext = receipt_ext(&sk_resp, &sk_sender, [7u8; 32], 1);
        s.apply_extrinsic(&ext, 1, NOW_MS, &auth_pid).unwrap();
        assert!(s.reputation.rateable.contains_key(&[7u8; 32]));

        let rate = |verdict, nonce| {
            Extrinsic::sign(
                &sk_sender,
                crate::extrinsic::PALLET_REPUTATION,
                &ReputationCall::Rate {
                    query_digest: [7u8; 32],
                    verdict,
                },
                sender,
                nonce,
            )
        };
        let unjustified = rate(
            Verdict::Dislike {
                reason: DislikeReason::Incorrect,
                note: String::new(),
            },
            2,
        );
        assert!(s
            .apply_extrinsic(&unjustified, 2, NOW_MS, &auth_pid)
            .is_err());
        let justified = rate(
            Verdict::Dislike {
                reason: DislikeReason::Incorrect,
                note: "the total it computed is wrong".into(),
            },
            3,
        );
        s.apply_extrinsic(&justified, 2, NOW_MS, &auth_pid).unwrap();
        let q = s.reputation.quality(&resp, 2);
        assert_eq!(q.dislikes, 1);
        assert!(q.percent < 50);
        assert!(s
            .apply_extrinsic(&rate(Verdict::Like, 4), 3, NOW_MS, &auth_pid)
            .is_err());
    }

    fn finality_ext(sk: &SigningKey, call: crate::finality::FinalityCall, nonce: u64) -> Extrinsic {
        Extrinsic::sign(
            sk,
            crate::extrinsic::PALLET_FINALITY,
            &call,
            sk.verifying_key().to_bytes(),
            nonce,
        )
    }

    #[test]
    fn a_justified_block_is_final_and_no_fork_can_undo_it() {
        use crate::finality::{FinalityCall, FinalityVote, Justification};
        let (mut s, founder, partner) = two_validators();
        let base = s.clone();
        s.apply_block(next_block(&s, 5, &partner)).unwrap();
        let target = s.head_hash();
        let submitter = fresh_signer().0;

        let one = Justification::from_votes(2, target, &[FinalityVote::sign(&founder, 2, target)]);
        let refused = finality_ext(&submitter, FinalityCall::Finalize { justification: one }, 1);
        assert_eq!(
            s.apply_extrinsic(&refused, 3, NOW_MS, &[0u8; 32]),
            Err(ChainError::BadJustification),
            "one of two validators is not a supermajority"
        );

        let votes = [
            FinalityVote::sign(&founder, 2, target),
            FinalityVote::sign(&partner, 2, target),
        ];
        let both = Justification::from_votes(2, target, &votes);
        let ext = finality_ext(
            &submitter,
            FinalityCall::Finalize {
                justification: both,
            },
            2,
        );
        let mut block = empty_block_on_top_of(&s, 3, 6);
        block.body.extrinsics.push(ext);
        let block = seal_with(&s, block, &founder);
        s.apply_block(block).unwrap();
        assert_eq!(s.finalized(), (2, target));

        let mut rival = base;
        let mut fork = Vec::new();
        for slot in [8u64, 10, 12, 14] {
            let b = next_block(&rival, slot, &founder);
            rival.apply_block(b.clone()).unwrap();
            fork.push(b);
        }
        assert!(matches!(
            s.clone().try_reorg(fork, 0),
            Err(ChainError::BelowFinality)
        ));
    }

    #[test]
    fn an_equivocating_validator_is_removed_but_founder_zero_is_only_recorded() {
        use crate::finality::{EquivocationProof, FinalityCall, FinalityVote};
        let (mut s, founder, partner) = two_validators();
        let reporter = fresh_signer().0;
        let proof = |k: &SigningKey| EquivocationProof {
            first: FinalityVote::sign(k, 16, [1u8; 32]),
            second: FinalityVote::sign(k, 16, [2u8; 32]),
        };
        let report = |k: &SigningKey, nonce| {
            finality_ext(
                &reporter,
                FinalityCall::ReportEquivocation {
                    proof: Box::new(proof(k)),
                },
                nonce,
            )
        };
        s.apply_extrinsic(&report(&founder, 1), 3, NOW_MS, &[0u8; 32])
            .unwrap();
        assert!(s.authority.is_founder(&founder.verifying_key().to_bytes()));
        assert!(s
            .apply_extrinsic(&report(&founder, 2), 3, NOW_MS, &[0u8; 32])
            .is_err());

        let partner_pid = partner.verifying_key().to_bytes();
        s.apply_extrinsic(&report(&partner, 3), 3, NOW_MS, &[0u8; 32])
            .unwrap();
        assert!(!s.authority.is_authority_signer(&partner_pid));
        assert_eq!(s.validator_set().len(), 1);
    }

    #[test]
    fn a_justification_is_checked_against_the_set_at_its_height() {
        use crate::finality::{FinalityCall, FinalityVote, Justification};
        let founder = producer_key();
        let partner = SigningKey::from_bytes(&[0x43u8; 32]);
        let mut s = test_chain();
        s.apply_block(next_block(&s, 1, &founder)).unwrap();
        let s_before = s.hash_at(1).unwrap();
        let add = authority_ext(
            &founder,
            founder.verifying_key().to_bytes(),
            AuthorityCall::Add {
                pubkey: partner.verifying_key().to_bytes(),
                channels: vec![],
                attestation_hash: [0u8; 32],
            },
            1,
        );
        let mut b = empty_block_on_top_of(&s, 2, 2);
        b.body.extrinsics.push(add);
        let b = seal_with(&s, b, &founder);
        s.apply_block(b).unwrap();
        assert_eq!(s.validator_set().len(), 2);
        assert_eq!(s.authority.validators_at(1).len(), 1);
        let j =
            Justification::from_votes(1, s_before, &[FinalityVote::sign(&founder, 1, s_before)]);
        let submitter = fresh_signer().0;
        let ext = finality_ext(&submitter, FinalityCall::Finalize { justification: j }, 1);
        s.apply_extrinsic(&ext, 3, NOW_MS, &[0u8; 32]).unwrap();
        assert_eq!(
            s.finalized().0,
            1,
            "one vote was a supermajority of the set at height 1"
        );
        let _ = partner;
    }

    #[test]
    fn blocks_serialize_as_appendable_records() {
        let key = producer_key();
        let mut s = test_chain();
        for slot in 1..=5u64 {
            s.apply_block(next_block(&s, slot, &key)).unwrap();
        }
        let mut file = s.serialize_blocks_after(0);
        let head = s.head_height();
        s.apply_block(next_block(&s, 6, &key)).unwrap();
        file.extend_from_slice(&s.serialize_blocks_after(head));
        let blocks = ChainState::deserialize_blocks(&file).unwrap();
        assert_eq!(blocks.len(), 6);
        let replayed = {
            let mut r = test_chain();
            for b in blocks {
                r.apply_block(b).unwrap();
            }
            r
        };
        assert_eq!(replayed.head_hash(), s.head_hash());
        assert!(ChainState::deserialize_blocks(&file[..file.len() - 1]).is_none());
    }

    // ---- identity: a wallet and the nodes that work for it ----

    fn delegate_ext(wallet_sk: &SigningKey, node_sk: &SigningKey, nonce: u64) -> Extrinsic {
        let wallet = wallet_sk.verifying_key().to_bytes();
        Extrinsic::sign(
            wallet_sk,
            crate::extrinsic::PALLET_IDENTITY,
            &IdentityCall::Delegate {
                node: node_sk.verifying_key().to_bytes(),
                node_sig: crate::pallets::identity::sign_delegation(node_sk, &wallet),
            },
            wallet,
            nonce,
        )
    }

    /// The state root as it was computed before the Identity pallet existed.
    fn root_before_identity(s: &ChainState) -> [u8; 32] {
        let roots = [
            SystemPallet::pallet_root(&s.system),
            BalancesPallet::pallet_root(&s.balances),
            ContributionsPallet::pallet_root(&s.contributions),
            NaiPallet::pallet_root(&s.nai),
            crate::pallets::market::MarketPallet::pallet_root(&s.market),
            crate::pallets::authority::AuthorityPallet::pallet_root(&s.authority),
            crate::pallets::economy::EconomyPallet::pallet_root(&s.economy),
            GovernancePallet::pallet_root(&s.governance),
            ReputationPallet::pallet_root(&s.reputation),
        ];
        let mut h = Sha256::new();
        for (i, root) in roots.iter().enumerate() {
            if i > 0 {
                h.update(b"||");
            }
            h.update(root);
        }
        h.finalize().into()
    }

    #[test]
    fn without_delegations_the_root_is_the_one_sealed_before_identity_existed() {
        let (_, founder) = fresh_signer();
        let (wallet_sk, _) = fresh_signer();
        let (node_sk, _) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        s.balances.mint([3; 32], 7);
        assert_eq!(s.state_root(), root_before_identity(&s));

        s.apply_extrinsic(&delegate_ext(&wallet_sk, &node_sk, 1), 1, 0, &founder)
            .unwrap();
        assert_ne!(s.state_root(), root_before_identity(&s));
    }

    #[test]
    fn a_delegated_node_never_holds_credits() {
        let (_, founder) = fresh_signer();
        let (wallet_sk, wallet) = fresh_signer();
        let (node_sk, node) = fresh_signer();
        let (sender_sk, sender) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        s.balances.mint(node, 30);
        s.balances.mint(sender, 50);

        s.apply_extrinsic(&delegate_ext(&wallet_sk, &node_sk, 1), 1, 0, &founder)
            .unwrap();
        assert_eq!((s.balance_of(&node), s.balance_of(&wallet)), (0, 30));
        assert_eq!(s.account_of(&node), wallet);

        // Sent to the node, received by the wallet.
        let transfer = Extrinsic::sign(
            &sender_sk,
            PALLET_BALANCES,
            &BalancesCall::Transfer {
                to: node,
                amount: 20,
            },
            sender,
            1,
        );
        s.apply_extrinsic(&transfer, 2, 0, &founder).unwrap();
        assert_eq!((s.balance_of(&node), s.balance_of(&wallet)), (0, 50));

        // Earned by the node, paid to the wallet: the node redeems with its
        // own key, unattended.
        s.contributions.record_lottery_win(node, 2_000);
        let redeem = Extrinsic::sign(
            &node_sk,
            PALLET_CONTRIBUTIONS,
            &ContributionsCall::Redeem {
                receiver: node,
                shares: 0,
                lottery_dimension: 2_000,
            },
            node,
            1,
        );
        s.apply_extrinsic(&redeem, 3, 0, &founder).unwrap();
        assert_eq!((s.balance_of(&node), s.balance_of(&wallet)), (0, 52));

        // So the node key has nothing to spend.
        let spend = Extrinsic::sign(
            &node_sk,
            crate::extrinsic::PALLET_NAI,
            &NaiCall::Spend {
                session_token: [1; 32],
            },
            node,
            2,
        );
        assert!(s.apply_extrinsic(&spend, 4, 0, &founder).is_err());
        assert_eq!(s.balance_of(&wallet), 52);
    }

    #[test]
    fn a_node_uses_the_sessions_its_wallet_paid_for_and_no_others() {
        let (_, founder) = fresh_signer();
        let (wallet_sk, wallet) = fresh_signer();
        let (node_sk, node) = fresh_signer();
        let (other_sk, other) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        s.balances.mint(wallet, 100);
        s.balances.mint(other, 100);
        s.apply_extrinsic(&delegate_ext(&wallet_sk, &node_sk, 1), 1, 1_000, &founder)
            .unwrap();

        let spend = |sk: &SigningKey, pid: PeerId, token: [u8; 32], nonce: u64| {
            Extrinsic::sign(
                sk,
                crate::extrinsic::PALLET_NAI,
                &NaiCall::Spend {
                    session_token: token,
                },
                pid,
                nonce,
            )
        };
        s.apply_extrinsic(&spend(&wallet_sk, wallet, [1; 32], 2), 2, 1_000, &founder)
            .unwrap();
        s.apply_extrinsic(&spend(&other_sk, other, [2; 32], 1), 3, 1_000, &founder)
            .unwrap();

        let use_session = |token: [u8; 32], nonce: u64| {
            Extrinsic::sign(
                &node_sk,
                crate::extrinsic::PALLET_NAI,
                &NaiCall::UseSession {
                    session_token: token,
                },
                node,
                nonce,
            )
        };
        let before = s.nai.sessions[&[1; 32]].inferences_remaining;
        s.apply_extrinsic(&use_session([1; 32], 1), 4, 2_000, &founder)
            .unwrap();
        assert_eq!(s.nai.sessions[&[1; 32]].inferences_remaining, before - 1);
        assert!(s
            .apply_extrinsic(&use_session([2; 32], 2), 5, 2_000, &founder)
            .is_err());
    }

    #[test]
    fn the_founder_key_cannot_be_anyones_node() {
        let founder_sk = SigningKey::from_bytes(&[42; 32]);
        let founder = founder_sk.verifying_key().to_bytes();
        let (wallet_sk, _) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        let err = s
            .apply_extrinsic(&delegate_ext(&wallet_sk, &founder_sk, 1), 1, 0, &founder)
            .unwrap_err();
        assert!(matches!(err, ChainError::UnauthorizedAuthorityCall));
        assert_eq!(s.balance_of(&founder), TREASURY_GENESIS_AMOUNT);
        assert!(s.identity.is_empty());
    }

    #[test]
    fn a_wallet_stands_on_its_nodes_history_and_its_nodes_have_no_vote_of_their_own() {
        use crate::pallets::governance::voting_weight_group;
        use crate::pallets::market::check_eligible_group;

        let (_, founder) = fresh_signer();
        let (wallet_sk, wallet) = fresh_signer();
        let (node_sk, node) = fresh_signer();
        let mut s = ChainState::with_genesis_authority(founder);
        let day = 24 * 60 * 60 * 1_000u64;
        let now = 30 * day;
        // The node did the work; the wallet is new.
        s.contributions.first_seen_ms.insert(node, now - 20 * day);
        s.contributions.record_lottery_win(node, 5_000);

        let wallet_alone = [wallet];
        assert!(check_eligible_group(&s.contributions, &wallet_alone, now).is_err());
        assert_eq!(voting_weight_group(&s.contributions, &wallet_alone, now), 0);

        s.apply_extrinsic(&delegate_ext(&wallet_sk, &node_sk, 1), 1, now, &founder)
            .unwrap();
        let group = s.identity.group_of(&wallet);
        assert!(check_eligible_group(&s.contributions, &group, now).is_ok());
        assert!(voting_weight_group(&s.contributions, &group, now) > 0);
        // The node's weight is counted once, through its wallet.
        let node_group = s.identity.group_of(&node);
        assert_eq!(voting_weight_group(&s.contributions, &node_group, now), 0);
        assert!(check_eligible_group(&s.contributions, &node_group, now).is_err());
    }
}

#[cfg(test)]
mod upgrade_tests {
    use super::*;
    use crate::block::SLOT_DURATION_MS;
    use crate::pallets::upgrade::{UpgradeCall, SUPPORTED_OVERRIDE};
    use ed25519_dalek::SigningKey;

    fn founder() -> SigningKey {
        SigningKey::from_bytes(&[0x42u8; 32])
    }

    fn founder_pid() -> PeerId {
        founder().verifying_key().to_bytes()
    }

    fn chain() -> ChainState {
        ChainState::with_genesis_authority(founder_pid())
    }

    fn schedule(sk: &SigningKey, version: u32, at_ms: u64, nonce: u64) -> Extrinsic {
        Extrinsic::sign(
            sk,
            crate::extrinsic::PALLET_UPGRADE,
            &UpgradeCall::Schedule {
                version,
                activate_at_ms: at_ms,
                release: [9u8; 32],
            },
            sk.verifying_key().to_bytes(),
            nonce,
        )
    }

    /// A block at `slot` on top of `state`, sealed by the founder, with the
    /// state root a producer that implements `supported` would compute.
    fn block(state: &ChainState, slot: u64, extrinsics: Vec<Extrinsic>) -> Block {
        let key = founder();
        let mut b = Block {
            header: BlockHeader {
                version: 1,
                height: state.head_height() + 1,
                slot,
                parent_hash: state.head_hash(),
                state_root: [0u8; 32],
                extrinsics_root: [0u8; 32],
                lottery_root: [0u8; 32],
                author: crate::validators::authority_peer_id(&key.verifying_key()),
                signature: [0u8; 64],
                timestamp_ms: slot * SLOT_DURATION_MS,
            },
            body: BlockBody {
                extrinsics,
                lottery_results: Vec::new(),
            },
        };
        b.header.extrinsics_root = b.extrinsics_root();
        b.header.lottery_root = b.lottery_root();
        let mut probe = state.clone();
        probe
            .apply_block_unverified(b.clone())
            .expect("producer dry run");
        b.header.state_root = probe.state_root();
        b.header.sign(&key);
        b
    }

    const NOTICE: u64 = 10 * 60 * 1_000;
    const SLOT0: u64 = 900_000_000;

    /// A chain that scheduled v2 in block 1, plus that block and the time
    /// v2 activates.
    fn scheduled_chain() -> (ChainState, Vec<Block>, u64) {
        let mut s = chain();
        let t0 = SLOT0 * SLOT_DURATION_MS;
        let at = t0 + NOTICE;
        let b1 = block(&s, SLOT0, vec![schedule(&founder(), 2, at, 1)]);
        s.apply_block(b1.clone()).unwrap();
        (s, vec![b1], at)
    }

    #[test]
    fn only_the_founder_schedules_an_upgrade() {
        let mut s = chain();
        let other = SigningKey::from_bytes(&[0x11u8; 32]);
        let at = 1_000 + NOTICE;
        assert_eq!(
            s.apply_extrinsic(&schedule(&other, 2, at, 1), 1, 1_000, &founder_pid()),
            Err(ChainError::UnauthorizedAuthorityCall)
        );
        assert!(s.upgrade.is_default());
        s.apply_extrinsic(&schedule(&founder(), 2, at, 1), 1, 1_000, &founder_pid())
            .unwrap();
        assert_eq!(s.upgrade.scheduled.as_ref().unwrap().version, 2);
    }

    #[test]
    fn an_upgrade_needs_the_minimum_notice() {
        let mut s = chain();
        let err = s
            .apply_extrinsic(
                &schedule(&founder(), 2, 1_000 + NOTICE - 1, 1),
                1,
                1_000,
                &founder_pid(),
            )
            .unwrap_err();
        assert_eq!(err, ChainError::Pallet(PalletError::InvalidArgs));
    }

    #[test]
    fn the_rules_switch_at_the_first_block_at_or_after_the_time() {
        SUPPORTED_OVERRIDE.with(|c| c.set(Some(2)));
        let (mut s, _, at) = scheduled_chain();
        let before = block(&s, at / SLOT_DURATION_MS - 1, Vec::new());
        s.apply_block(before).unwrap();
        assert_eq!(s.rules_version(), 1, "one slot early");
        let on_time = block(&s, at / SLOT_DURATION_MS, Vec::new());
        let height = on_time.header.height;
        s.apply_block(on_time).unwrap();
        assert_eq!(s.rules_version(), 2);
        assert_eq!(s.upgrade.history, vec![(2, height)]);
        assert!(s.upgrade.scheduled.is_none());
    }

    #[test]
    fn a_build_without_the_new_rules_stops_without_touching_state_then_catches_up() {
        // The up-to-date producer seals the activation block.
        SUPPORTED_OVERRIDE.with(|c| c.set(Some(2)));
        let (producer, _, at) = scheduled_chain();
        let activation = block(&producer, at / SLOT_DURATION_MS, Vec::new());
        let mut upgraded = producer.clone();
        upgraded.apply_block(activation.clone()).unwrap();

        // A stale build stops at it, unchanged.
        SUPPORTED_OVERRIDE.with(|c| c.set(Some(1)));
        let mut stale = producer.clone();
        let root = stale.state_root();
        assert_eq!(
            stale.apply_block(activation.clone()),
            Err(ChainError::UpgradeRequired { version: 2 })
        );
        assert_eq!(stale.head_height(), producer.head_height());
        assert_eq!(stale.state_root(), root);
        assert!(stale.upgrade.scheduled_unsupported().is_some());

        // Given the new release, it applies the same block to the same state.
        SUPPORTED_OVERRIDE.with(|c| c.set(Some(2)));
        stale.apply_block(activation).unwrap();
        assert_eq!(stale.state_root(), upgraded.state_root());
        assert_eq!(stale.rules_version(), 2);
    }

    #[test]
    fn replay_from_genesis_reaches_the_same_activation() {
        SUPPORTED_OVERRIDE.with(|c| c.set(Some(2)));
        let (mut s, mut blocks, at) = scheduled_chain();
        for slot in [
            at / SLOT_DURATION_MS - 1,
            at / SLOT_DURATION_MS,
            at / SLOT_DURATION_MS + 5,
        ] {
            let b = block(&s, slot, Vec::new());
            s.apply_block(b.clone()).unwrap();
            blocks.push(b);
        }
        let mut replay = chain();
        for b in blocks {
            replay.apply_block(b).unwrap();
        }
        assert_eq!(replay.state_root(), s.state_root());
        assert_eq!(replay.upgrade, s.upgrade);
    }

    #[test]
    fn default_upgrade_storage_stays_out_of_the_state_root() {
        let mut s = chain();
        let root = s.state_root();
        s.upgrade
            .schedule(2, NOTICE, [0u8; 32], 0, 1, NOTICE)
            .unwrap();
        assert_ne!(s.state_root(), root, "a schedule is part of the state");
        s.upgrade.cancel().unwrap();
        assert_eq!(
            s.state_root(),
            root,
            "back to default, back to the old root"
        );
    }

    #[test]
    fn governance_schedules_upgrades_only_after_the_founder_resigns() {
        let action = || GovAction::ScheduleUpgrade {
            version: 2,
            activate_at_ms: 1_000 + NOTICE,
            release: [0u8; 32],
        };
        let enact = |s: &mut ChainState| {
            let mut system = s.system.clone();
            let mut ctx = ApplyCtx {
                block_height: 5,
                timestamp_ms: 1_000,
                author: &[0u8; 32],
                signer: &[0u8; 32],
                system: &mut system,
                current_shares_per_credit: 1_000,
                identity: &crate::pallets::NO_IDENTITY,
                rules_version: crate::pallets::upgrade::GENESIS_RULES_VERSION,
            };
            ChainState::enact(
                EnactTargets {
                    nai: &mut s.nai,
                    balances: &mut s.balances,
                    authority: &mut s.authority,
                    economy: &mut s.economy,
                    upgrade: &mut s.upgrade,
                },
                action(),
                &mut ctx,
            )
        };
        let mut s = chain();
        assert!(!enact(&mut s), "the founder is still active");
        assert!(s.upgrade.is_default());
        s.authority.dissolved_at = Some(4);
        assert!(enact(&mut s));
        assert_eq!(s.upgrade.scheduled.as_ref().unwrap().version, 2);
    }
}
