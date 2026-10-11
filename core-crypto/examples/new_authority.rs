// SPDX-License-Identifier: BUSL-1.1
// Generate a brand-new BIP39 mnemonic and derive the ed25519 identity from it
// in one step — used for chain genesis resets when a fresh authority key is
// needed (see derive_pubkey.rs for deriving from an existing phrase instead).
//
//   cargo run --example new_authority --release -p core-crypto

use core_crypto::mnemonic::NodeMnemonic;
use ed25519_dalek::SigningKey;

fn main() {
    let mnemonic = NodeMnemonic::generate();
    let phrase = mnemonic.phrase();
    let seed = mnemonic.to_seed();
    let signing_key = SigningKey::from_bytes(&seed);
    let pubkey = signing_key.verifying_key().to_bytes();

    println!("mnemonic (24 words — BACK THIS UP OFFLINE, this is the root key):");
    println!("{}", phrase);
    println!();
    println!("peer_id  / NEPHERAL_TEAM_PUBKEY  (bake at compile time, 64 hex):");
    println!("{}", hex::encode(pubkey));
    println!();
    println!("seed     / NEPHERAL_AUTHORITY_KEY (set as runtime env, 64 hex — SECRET):");
    println!("{}", hex::encode(seed));
    println!();
    println!("short fingerprint (first 8 bytes of peer_id):");
    println!("{}", hex::encode(&pubkey[..8]));
}
