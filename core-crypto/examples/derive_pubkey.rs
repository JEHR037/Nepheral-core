// SPDX-License-Identifier: BUSL-1.1
// Derive an ed25519 public key from a BIP39 mnemonic using Nepheral's
// own identity derivation pipeline.
//
//   cargo run --example derive_pubkey --release -- "word1 word2 ... word24"
//
// Output: 64 hex chars = 32-byte ed25519 verifying key = the peer_id this
// mnemonic will produce when fed to NodeIdentity::from_mnemonic.
// Bake this value as NEPHERAL_TEAM_PUBKEY at build time so the chain
// recognizes the matching node as authority.

use core_crypto::mnemonic::NodeMnemonic;
use ed25519_dalek::SigningKey;

fn main() {
    let phrase = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let phrase = phrase.trim();
    if phrase.is_empty() {
        eprintln!("usage: derive_pubkey \"<24 BIP39 words>\"");
        std::process::exit(2);
    }

    let mnemonic = match NodeMnemonic::from_phrase(phrase) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("invalid BIP39 mnemonic: {e}");
            std::process::exit(1);
        }
    };
    let seed = mnemonic.to_seed();
    let signing_key = SigningKey::from_bytes(&seed);
    let pubkey = signing_key.verifying_key().to_bytes();

    println!("peer_id  / NEPHERAL_TEAM_PUBKEY  (bake at compile time, 64 hex):");
    println!("{}", hex::encode(pubkey));
    println!();
    println!("seed     / NEPHERAL_AUTHORITY_KEY (set as runtime env, 64 hex — SECRET):");
    println!("{}", hex::encode(seed));
    println!();
    println!("short fingerprint (first 8 bytes of peer_id):");
    println!("{}", hex::encode(&pubkey[..8]));
}
