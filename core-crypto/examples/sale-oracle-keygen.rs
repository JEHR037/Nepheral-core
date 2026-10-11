// SPDX-License-Identifier: BUSL-1.1
//! Generates a sale-oracle key pair for the NAI satellite.
//!
//! Run this on the machine that will hold the key. The recovery phrase is
//! written straight to the file you name, never to stdout, so it does not pass
//! through a terminal, a shell history, a log or an operator's screen. Only the
//! public key is printed, which is what the authority announces on chain with
//! `SetSaleOracle`.
//!
//!     cargo run -p core-crypto --example sale-oracle-keygen -- /etc/nepheral/oracle.seed
//!
//! The key this produces can submit `DirectSale`, `Settle`, `FailClaim` and `SetSaleHealth` and nothing else,
//! and the authority can revoke it in a single block. It is not the
//! block-producing authority seed and must never be confused with it.
use std::io::Write;

use core_crypto::NodeMnemonic;
use ed25519_dalek::SigningKey;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: sale-oracle-keygen <path-to-write-recovery-phrase>");
        eprintln!();
        eprintln!("The phrase is written to that path and never printed.");
        std::process::exit(2);
    };

    if std::path::Path::new(&path).exists() {
        eprintln!("refusing to overwrite an existing file: {path}");
        eprintln!("A key may already be in use there. Move it aside deliberately.");
        std::process::exit(1);
    }

    let mnemonic = NodeMnemonic::generate();
    let signing = SigningKey::from_bytes(&mnemonic.to_seed());
    let pubkey = signing.verifying_key().to_bytes();

    if let Err(e) = write_private(&path, mnemonic.phrase()) {
        eprintln!("could not write the recovery phrase to {path}: {e}");
        std::process::exit(1);
    }

    println!("{}", hex::encode(pubkey));
    eprintln!();
    eprintln!("Recovery phrase written to {path} (owner-readable only).");
    eprintln!("Back it up offline: losing it means the satellite cannot mint, and");
    eprintln!("the authority must appoint a replacement oracle.");
    eprintln!();
    eprintln!("The line printed above is the PUBLIC key. Set it on the node as");
    eprintln!("NEPHERAL_SALE_ORACLE_PUBKEY so the authority announces it on chain.");
}

/// Writes owner-only from the moment the file exists, rather than creating it
/// world-readable and tightening afterwards — a window in which another local
/// process could read the phrase.
fn write_private(path: &str, phrase: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    writeln!(f, "{phrase}")?;
    f.sync_all()
}
