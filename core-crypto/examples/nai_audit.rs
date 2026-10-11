// SPDX-License-Identifier: BUSL-1.1
// Replays a chain file, verifying every block, and reports the economy: who
// holds credits, what each node has earned and could redeem, and the state of
// sales, sessions and the emission controller.
//
//   cargo run -q --example nai_audit -p core-crypto -- <path/to/chain.bin> [--full]
//
// --full prints whole peer ids instead of their first four bytes.

use std::collections::BTreeSet;

use core_crypto::ChainState;

fn short(p: &[u8; 32]) -> String {
    hex::encode(&p[..4])
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: nai_audit <chain.bin> [--full]");
    let full = std::env::args().any(|a| a == "--full");
    let id = |p: &[u8; 32]| if full { hex::encode(p) } else { short(p) };
    let data = std::fs::read(&path).expect("read chain file");
    let body = core_crypto::chain_file::strip_header(&data).expect("not a current chain file");
    let blocks = ChainState::deserialize_blocks(body).expect("blocks decode");
    let n_blocks = blocks.len();
    // Who lands the justifications, and who authors the blocks they ride in.
    let mut finalized_by: std::collections::BTreeMap<[u8; 32], u64> = Default::default();
    let mut authored: std::collections::BTreeMap<[u8; 32], u64> = Default::default();
    for b in &blocks {
        *authored.entry(b.header.author).or_default() += 1;
        for ext in &b.body.extrinsics {
            if let Ok(core_crypto::extrinsic::DecodedCall::Finality(
                core_crypto::finality::FinalityCall::Finalize { .. },
            )) = core_crypto::extrinsic::DecodedCall::decode(ext.pallet_idx, &ext.args)
            {
                *finalized_by.entry(ext.signer).or_default() += 1;
            }
        }
    }
    let s = ChainState::replay_blocks(blocks);
    assert_eq!(
        s.head_height() as usize,
        n_blocks,
        "replay stopped early - a block failed to verify"
    );

    let founder = s.authority.founder;
    let is_founder = |p: &[u8; 32]| founder.as_ref() == Some(p);
    let (fin_h, _) = s.finalized();
    println!(
        "height {}  finalized {}  ({} blocks replayed and verified)\n",
        s.head_height(),
        fin_h,
        n_blocks
    );

    let e = &s.economy;
    let span_min = match (e.recent_activity.front(), e.recent_activity.back()) {
        (Some((a, _)), Some((b, _))) => (b - a) / 60_000,
        _ => 0,
    };
    let window_equivs: u64 = e.recent_activity.iter().map(|(_, q)| q).sum();
    println!("EMISSION");
    println!(
        "  shares per credit {}   target {} credits/hour",
        e.current_shares_per_credit, e.target_credits_per_hour
    );
    println!(
        "  window: {} blocks over {} min, {} share-equivalents",
        e.recent_activity.len(),
        span_min,
        window_equivs
    );

    let n = &s.nai;
    println!("\nNAI SUPPLY");
    println!(
        "  minted {}   burned {}   minted today {} / cap {}",
        n.total_minted, n.total_spent, n.daily_minted, n.daily_mint_cap
    );
    println!(
        "  sessions on chain {}   direct sales {}   sale oracle {}   sale wallet {}",
        n.sessions.len(),
        n.direct_sales.len(),
        n.sale_oracle
            .as_ref()
            .map(short)
            .unwrap_or_else(|| "none".into()),
        n.direct_sale_wallet.as_deref().unwrap_or("none")
    );
    println!(
        "  direct sale {}{}   oracle nonce {}",
        if n.direct_sale_healthy {
            "healthy"
        } else {
            "paused"
        },
        n.direct_sale_health_reason
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default(),
        n.sale_oracle
            .and_then(|o| s.system.nonces.get(&o).copied())
            .unwrap_or(0)
    );
    println!(
        "  market: {} asks, {} bids, {} claims, {} fills, {} escrowed",
        s.market.asks.len(),
        s.market.bids.len(),
        s.market.claims.len(),
        s.market.fills.len(),
        s.market.escrowed
    );
    println!("  top-tier session costs {:?}", n.top_tier_session_costs);
    println!("  pending parameter changes {}", n.pending_changes.len());
    let head_ms = s.blocks.last().map(|b| b.header.timestamp_ms).unwrap_or(0);
    let mut sessions: Vec<_> = n.sessions.iter().collect();
    sessions.sort_by_key(|(_, x)| x.expires_at_ms);
    for (token, x) in sessions {
        println!(
            "  session {}  owner {}  channel {}  {} inferences left  {}",
            hex::encode(&token[..4]),
            id(&x.owner),
            x.channel.as_deref().unwrap_or("priority"),
            x.inferences_remaining,
            if x.expires_at_ms > head_ms {
                format!("expires in {} min", (x.expires_at_ms - head_ms) / 60_000)
            } else {
                "expired".to_string()
            }
        );
    }

    let mut peers: BTreeSet<[u8; 32]> = BTreeSet::new();
    peers.extend(s.balances.balances.keys().copied());
    peers.extend(s.contributions.wins_by_peer.keys().copied());
    peers.extend(s.contributions.shares_consumed.keys().copied());
    peers.extend(s.authority.authorities.iter().copied());

    println!("\nBLOCKS AND FINALITY");
    for (p, n) in &authored {
        println!("  {} authored {n} blocks", id(p));
    }
    for (p, n) in &finalized_by {
        println!("  {} landed {n} justifications", id(p));
    }

    let ratio = e.current_shares_per_credit.max(1);
    println!("\nPEERS");
    println!(
        "  {:8}  {:>10}  {:>5}  {:>6}  {:>9}  {:>11}  {:>8}  {:>5}  role",
        "peer", "balance", "wins", "asked", "earned", "redeemable", "=> NAI", "nonce"
    );
    for p in &peers {
        let (shares, dim) = s.redeemable(p);
        let earned = s
            .contributions
            .lottery_dimension_by_peer
            .get(p)
            .copied()
            .unwrap_or(0);
        let role = if is_founder(p) {
            "founder 0"
        } else if s.authority.is_authority_signer(p) {
            "founding node"
        } else {
            ""
        };
        let channels = s
            .authority
            .metadata
            .get(p)
            .map(|m| m.channels.join(","))
            .unwrap_or_default();
        println!(
            "  {}  {:>10}  {:>5}  {:>6}  {:>9}  {:>11}  {:>8}  {:>5}  {} {}",
            id(p),
            s.balance_of(p),
            s.wins_of(p),
            s.consumed_of(p),
            earned,
            shares + dim,
            (shares + dim) / ratio,
            s.system.nonces.get(p).copied().unwrap_or(0),
            role,
            if channels.is_empty() {
                String::new()
            } else {
                format!("[{channels}]")
            }
        );
    }
}
