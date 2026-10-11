// SPDX-License-Identifier: BUSL-1.1
//! First-run interactive setup for the headless `nepheral-node` binary.
//! Priority: `NEPHERAL_NODE_MNEMONIC` env vars, then a saved
//! `node_setup_done` config, then an interactive wizard on a real TTY.
//! Falls back to defaults rather than hang if none apply.

use std::io::{IsTerminal, Write};
use std::net::SocketAddr;

use app_kernel::nepheral_config::{self, NepheralConfig};

pub struct ResolvedNode {
    pub port: u16,
    pub mnemonic: core_crypto::NodeMnemonic,
    pub is_bootstrap: bool,
    pub bootstrap_peers: Vec<SocketAddr>,
}

pub fn resolve() -> Result<ResolvedNode, String> {
    if let Ok(phrase) = std::env::var("NEPHERAL_NODE_MNEMONIC") {
        let mnemonic = core_crypto::NodeMnemonic::from_phrase(phrase.trim())
            .map_err(|e| format!("Invalid NEPHERAL_NODE_MNEMONIC: {e}"))?;
        return Ok(from_env(mnemonic));
    }

    let mut cfg = nepheral_config::load_config().unwrap_or_default();

    if !cfg.node_setup_done && std::io::stdin().is_terminal() {
        run_wizard(&mut cfg)?;
        cfg.node_setup_done = true;
        nepheral_config::save_config(&cfg)?;
    }

    let mnemonic = nepheral_config::load_or_create_identity_mnemonic()?;
    let bootstrap_peers = parse_peer_list(&cfg.node_bootstrap_peers.join(","));

    Ok(ResolvedNode {
        // Once node_setup_done is true, cfg.node_port is a deliberate,
        // persisted choice (the wizard, or a prior env-driven run) and must
        // not be second-guessed by whatever NEPHERAL_PORT happens to be set
        // to on this particular restart. Only a fresh, never-configured
        // node (no wizard possible: no TTY) falls through here, and for
        // that case NEPHERAL_PORT is the only way to configure it at all.
        port: if cfg.node_setup_done {
            cfg.node_port
        } else {
            std::env::var("NEPHERAL_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(cfg.node_port)
        },
        mnemonic,
        is_bootstrap: cfg.node_is_bootstrap,
        bootstrap_peers,
    })
}

fn from_env(mnemonic: core_crypto::NodeMnemonic) -> ResolvedNode {
    let port = std::env::var("NEPHERAL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(443);
    let is_bootstrap = std::env::var("NEPHERAL_IS_BOOTSTRAP")
        .map(|v| v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let bootstrap_peers =
        parse_peer_list(&std::env::var("NEPHERAL_BOOTSTRAP_PEER").unwrap_or_default());
    ResolvedNode {
        port,
        mnemonic,
        is_bootstrap,
        bootstrap_peers,
    }
}

fn parse_peer_list(raw: &str) -> Vec<SocketAddr> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect()
}

fn prompt(label: &str, default: &str) -> String {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return default.to_string();
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn prompt_yes_no(label: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "Y/n" } else { "y/N" };
    let answer = prompt(
        &format!("{label} ({hint})"),
        if default_yes { "y" } else { "n" },
    );
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

fn run_wizard(cfg: &mut NepheralConfig) -> Result<(), String> {
    println!();
    println!(
        "\u{25c7} Nepheral node setup — this runs once. Edit ~/.nepheral/config.json to change"
    );
    println!("  anything afterwards, or delete it to be asked again.");
    println!();

    let identity_path = nepheral_config::nepheral_dir().join("identity.mnemonic");
    if !identity_path.exists() {
        if prompt_yes_no(
            "Restore an existing 24-word recovery phrase (instead of creating a new identity)?",
            false,
        ) {
            loop {
                let phrase = prompt("Paste the 24 words, space-separated", "");
                match core_crypto::NodeMnemonic::from_phrase(phrase.trim()) {
                    Ok(_) => {
                        let dir = nepheral_config::nepheral_dir();
                        nepheral_config::ensure_private_dir(&dir)?;
                        nepheral_config::write_private_file(
                            &identity_path,
                            phrase.trim().as_bytes(),
                        )?;
                        println!("  Identity restored.");
                        break;
                    }
                    Err(e) => println!("  That isn't a valid phrase ({e}) — try again."),
                }
            }
        } else {
            println!("  A new identity will be generated and saved to");
            println!("  {}", identity_path.display());
            println!("  Back up those 24 words — whoever holds them controls this node's wallet.");
        }
    } else {
        println!(
            "  Using the existing identity at {}",
            identity_path.display()
        );
    }
    println!();

    let port_str = prompt("Port to listen on (QUIC/UDP)", &cfg.node_port.to_string());
    cfg.node_port = port_str.parse().unwrap_or(cfg.node_port);

    cfg.node_is_bootstrap = prompt_yes_no(
        "Act as a bootstrap/relay node? (other peers dial this one to join the mesh and\n  get everyone's peer list via gossip)",
        false,
    );

    let default_peers = cfg.node_bootstrap_peers.join(",");
    let peers_raw = prompt(
        "Other node(s) to connect to on startup, comma-separated ip:port (blank = none)",
        &default_peers,
    );
    cfg.node_bootstrap_peers = peers_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    for p in &cfg.node_bootstrap_peers {
        if p.parse::<SocketAddr>().is_err() {
            println!("  Warning: '{p}' doesn't look like ip:port — it will be ignored at startup.");
        }
    }

    println!();
    println!("Setup complete — starting the node now. This will not ask again on restart.");
    println!();
    Ok(())
}
