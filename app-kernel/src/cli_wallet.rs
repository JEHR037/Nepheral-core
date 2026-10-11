// SPDX-License-Identifier: BUSL-1.1
//! `nepheral-node`'s wallet commands.
//!
//! The node runs with its own key and serves the network without anyone
//! present. The wallet - the account that holds the funds - lives in
//! `~/.nepheral/vault.bin` under a password, and these commands manage it:
//!
//!   nepheral-node wallet init       create a wallet (24 words + password)
//!   nepheral-node wallet restore    replace it from 24 words
//!   nepheral-node wallet password   change its password
//!   nepheral-node unlock            open it inside the running node
//!   nepheral-node lock              close it again
//!   nepheral-node status            node, wallet, balance, lock state
//!   nepheral-node wallet send <address> <amount>
//!
//! `init`, `restore` and `password` work on the file and need no running
//! node; the rest talk to it over its control socket.

use std::io::Write;

use app_kernel::nepheral_config;
use app_kernel::vault::{self, KdfCost, Secrets, Vault};
use zeroize::Zeroizing;

use crate::control::{self, Request};

/// Runs a wallet command if the arguments name one. `None` means none was
/// named and the node should start.
pub fn dispatch() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["unlock"] => unlock(),
        ["lock"] => simple(Request::Lock, "Wallet locked."),
        ["status"] => status(),
        ["health"] => health(),
        ["wallet", "init"] => init(None),
        ["wallet", "restore"] => restore(),
        ["wallet", "password"] => change_password(),
        ["wallet", "send", to, amount] => send(to, amount),
        ["help"] | ["--help"] | ["-h"] => {
            println!("{}", HELP);
            Ok(())
        }
        _ => return None,
    };
    Some(match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    })
}

const HELP: &str = "\
nepheral-node                      start the node (serves the network locked)
nepheral-node status               node, wallet, balance and lock state
nepheral-node health               chain progress and peers, as JSON
nepheral-node unlock               open the wallet inside the running node
nepheral-node lock                 close it again
nepheral-node wallet init          create a wallet: 24 words and a password
nepheral-node wallet restore       replace the wallet from its 24 words
nepheral-node wallet password      change the wallet's password
nepheral-node wallet send <address> <amount>

Locked, the node keeps serving the network; spending, the local API and its
chats wait for `unlock`. It locks itself again after NEPHERAL_AUTO_LOCK_MINUTES
of inactivity (15 by default, 0 for never).";

fn read_password(label: &str) -> Result<Zeroizing<String>, String> {
    rpassword::prompt_password(label)
        .map(Zeroizing::new)
        .map_err(|e| format!("cannot read the password: {e}"))
}

fn new_password() -> Result<Zeroizing<String>, String> {
    loop {
        let first = read_password("New password (at least 8 characters): ")?;
        if first.chars().count() < vault::MIN_PASSWORD_CHARS {
            println!("  Too short.");
            continue;
        }
        let second = read_password("Repeat it: ")?;
        if *first == *second {
            return Ok(first);
        }
        println!("  They do not match.");
    }
}

fn ask(label: &str) -> String {
    print!("{label}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim().to_string()
}

fn yes(label: &str) -> bool {
    matches!(
        ask(label).to_ascii_lowercase().as_str(),
        "y" | "yes" | "s" | "si" | "sí"
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The API token a new wallet takes: the one the node already had, so tools
/// pointed at it keep working, or a fresh one. It moves out of config.json.
fn api_token_for_new_wallet() -> String {
    nepheral_config::load_config()
        .and_then(|c| c.local_api_server_token)
        .filter(|t| t.trim().len() >= 32)
        .unwrap_or_else(app_kernel::wallet::new_api_token)
}

/// Records the wallet's public address and the hash of its API token; the
/// token itself stays in the vault.
fn remember_wallet(mnemonic: &core_crypto::NodeMnemonic, token: &str) -> Result<String, String> {
    let address = hex::encode(app_kernel::wallet::WalletKey::address_of(mnemonic));
    let addr = address.clone();
    nepheral_config::update(move |c| c.wallet_address = Some(addr))?;
    app_kernel::wallet::store_api_token_hash(&app_kernel::wallet::api_token_hash(token))?;
    Ok(address)
}

fn init(phrase: Option<core_crypto::NodeMnemonic>) -> Result<(), String> {
    let dir = nepheral_config::nepheral_dir();
    nepheral_config::ensure_private_dir(&dir)?;
    let restoring = phrase.is_some();
    if !restoring && dir.join(vault::VAULT_FILE).exists() {
        return Err("a wallet already exists here; `wallet restore` replaces it".into());
    }
    let mnemonic = match phrase {
        Some(m) => m,
        None => {
            let m = core_crypto::NodeMnemonic::generate();
            println!();
            println!("Your wallet's recovery phrase - shown this once:");
            println!();
            for (i, chunk) in m
                .phrase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .chunks(6)
                .enumerate()
            {
                let row: Vec<String> = chunk
                    .iter()
                    .enumerate()
                    .map(|(j, w)| format!("{:>2} {:<10}", i * 6 + j + 1, w))
                    .collect();
                println!("  {}", row.join(" "));
            }
            println!();
            println!("Write them on paper and keep it somewhere safe. They are the only way back");
            println!("in if the password is lost; whoever has them controls the wallet.");
            println!();
            if !yes("Have you written them down? (y/N) ") {
                return Err("nothing saved; run `wallet init` again when ready".into());
            }
            // Off the screen before going on.
            print!("\x1b[2J\x1b[H");
            m
        }
    };
    let password = new_password()?;
    let token = Zeroizing::new(api_token_for_new_wallet());
    let secrets = Secrets {
        mnemonic: mnemonic.phrase().to_string(),
        api_token: Some(token.to_string()),
        system_prompt: None,
        node_seed: None,
    };
    if restoring {
        vault::replace(&dir, &password, &secrets, KdfCost::DEFAULT, now_ms())
            .map_err(|e| e.to_string())?;
    } else {
        Vault::create(&dir, &password, &secrets, KdfCost::DEFAULT).map_err(|e| e.to_string())?;
    }
    let address = remember_wallet(&mnemonic, &token)?;
    println!(
        "Wallet {address} sealed in {}.",
        dir.join(vault::VAULT_FILE).display()
    );
    println!("Run `nepheral-node unlock` (with the node running) to link this node to it.");
    Ok(())
}

fn restore() -> Result<(), String> {
    let phrase = Zeroizing::new(ask("Type the 24 words, separated by spaces: "));
    print!("\x1b[1A\x1b[2K");
    let mnemonic = core_crypto::NodeMnemonic::from_phrase(
        &phrase.split_whitespace().collect::<Vec<_>>().join(" "),
    )
    .map_err(|e| format!("that is not a valid 24-word phrase ({e})"))?;
    if nepheral_config::nepheral_dir()
        .join(vault::VAULT_FILE)
        .exists()
    {
        println!("The current wallet will be kept beside the new one as vault.bin.bak-<time>.");
    }
    init(Some(mnemonic))
}

fn change_password() -> Result<(), String> {
    let dir = nepheral_config::nepheral_dir();
    let old = read_password("Current password: ")?;
    let (mut vault, secrets) = Vault::unlock(&dir, &old).map_err(|e| e.to_string())?;
    let new = new_password()?;
    vault
        .change_password(&old, &new, &secrets)
        .map_err(|e| e.to_string())?;
    println!("Password changed. A running node keeps the wallet as it was opened.");
    Ok(())
}

fn print_status(s: &control::Status) {
    println!("node      {}", s.node);
    match &s.wallet {
        Some(w) => println!("wallet    {w}"),
        None => println!("wallet    none - `nepheral-node wallet init` creates one"),
    }
    println!("balance   {} N-AI", s.balance);
    println!("link      {}", s.delegation);
    println!(
        "lock      {}{}",
        if s.unlocked { "unlocked" } else { "locked" },
        if s.auto_lock_minutes > 0 {
            format!(" (locks after {} min idle)", s.auto_lock_minutes)
        } else {
            String::new()
        }
    );
    if let Some(t) = &s.api_token {
        println!("api token {t}");
    }
}

fn reply_status(reply: control::Reply) -> Result<control::Status, String> {
    if !reply.ok {
        return Err(reply.error.unwrap_or_else(|| "refused".into()));
    }
    reply.status.ok_or_else(|| "no status in the reply".into())
}

fn status() -> Result<(), String> {
    print_status(&reply_status(control::call(&Request::Status)?)?);
    Ok(())
}

fn health() -> Result<(), String> {
    let reply = control::call(&Request::Health)?;
    if !reply.ok {
        return Err(reply.error.unwrap_or_else(|| "refused".into()));
    }
    let health = reply.health.ok_or("no health in the reply")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&health).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn simple(req: Request, done: &str) -> Result<(), String> {
    reply_status(control::call(&req)?)?;
    println!("{done}");
    Ok(())
}

fn unlock() -> Result<(), String> {
    let password = read_password("Wallet password: ")?;
    let reply = control::call(&Request::Unlock {
        password: password.to_string(),
    })?;
    let s = reply_status(reply)?;
    println!("Wallet unlocked.");
    print_status(&s);
    Ok(())
}

fn send(to: &str, amount: &str) -> Result<(), String> {
    let amount: u64 = amount
        .parse()
        .map_err(|_| "the amount is a whole number of N-AI")?;
    if to.len() != 64 || !to.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the recipient is a 64-character hex address".into());
    }
    let s = reply_status(control::call(&Request::Status)?)?;
    if !s.unlocked {
        return Err("the wallet is locked: run `nepheral-node unlock` first".into());
    }
    println!();
    println!("  Send      {amount} N-AI");
    println!("  To        {to}");
    println!(
        "  Balance   {} -> {}",
        s.balance,
        s.balance.saturating_sub(amount)
    );
    println!();
    if !yes("Confirm? (y/N) ") {
        return Err("not sent".into());
    }
    reply_status(control::call(&Request::Send {
        to: to.to_string(),
        amount,
    })?)?;
    println!("Sent; it lands with the next block.");
    Ok(())
}
