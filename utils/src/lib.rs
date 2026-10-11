// SPDX-License-Identifier: BUSL-1.1
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(crate::env_filter())
        .init();
}

/// The crates whose `info` output is worth having without being asked for.
/// Applied only as a default: a directive added after `from_default_env`
/// overrides `RUST_LOG` for the same target, which silently made
/// `RUST_LOG=app_kernel=debug` a no-op and left no way to raise verbosity.
pub const DEFAULT_TARGETS: &[&str] = &[
    "nepheral",
    "nepheral_desktop",
    "app_kernel",
    "ai_engine",
    "mesh_network",
    "core_crypto",
];

pub fn env_filter() -> tracing_subscriber::EnvFilter {
    let requested = std::env::var("RUST_LOG").unwrap_or_default();
    let mut filter = tracing_subscriber::EnvFilter::from_default_env();
    for target in DEFAULT_TARGETS {
        if mentions_target(&requested, target) {
            continue;
        }
        filter = filter.add_directive(format!("{target}=info").parse().expect("static directive"));
    }
    filter
}

/// True when `RUST_LOG` already says something about this target, in which
/// case the caller's wish wins over the default.
fn mentions_target(requested: &str, target: &str) -> bool {
    requested.split(',').any(|d| {
        let name = d.split('=').next().unwrap_or_default().trim();
        // A bare level ("debug") applies to everything, so it counts too.
        name == target || (!name.is_empty() && !d.contains('=') && !name.contains("::"))
    })
}

#[cfg(test)]
mod tests {
    use super::mentions_target;

    #[test]
    fn explicit_target_wins_over_the_default() {
        assert!(mentions_target("app_kernel=debug", "app_kernel"));
        assert!(mentions_target("info,app_kernel=trace", "app_kernel"));
    }

    #[test]
    fn unrelated_targets_keep_their_default() {
        assert!(!mentions_target("app_kernel=debug", "mesh_network"));
        assert!(!mentions_target("", "app_kernel"));
    }

    /// A bare level is a global request, so no per-target default should
    /// override it.
    #[test]
    fn bare_level_covers_every_target() {
        assert!(mentions_target("debug", "app_kernel"));
        assert!(mentions_target("debug", "mesh_network"));
    }
}
