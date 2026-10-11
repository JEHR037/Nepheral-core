// SPDX-License-Identifier: BUSL-1.1
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CredsMode {
    Cross {
        #[serde(default = "default_standard_deadline_ms")]
        standard_deadline_ms: u64,
        #[serde(default = "default_total_deadline_ms")]
        total_deadline_ms: u64,
        #[serde(default = "default_escalation_channel")]
        escalation_channel: String,
    },
    StandardOnly {
        #[serde(default = "default_total_deadline_ms")]
        total_deadline_ms: u64,
    },
    TopTierOnly {
        #[serde(default = "default_escalation_channel")]
        channel: String,
        #[serde(default = "default_total_deadline_ms")]
        total_deadline_ms: u64,
    },
    LocalOnly,
}

fn default_standard_deadline_ms() -> u64 {
    8_000
}
fn default_total_deadline_ms() -> u64 {
    30_000
}
fn default_escalation_channel() -> String {
    "chat".to_string()
}

impl Default for CredsMode {
    fn default() -> Self {
        CredsMode::Cross {
            standard_deadline_ms: default_standard_deadline_ms(),
            total_deadline_ms: default_total_deadline_ms(),
            escalation_channel: default_escalation_channel(),
        }
    }
}

impl CredsMode {
    pub fn human_summary(&self) -> String {
        match self {
            CredsMode::Cross {
                standard_deadline_ms,
                total_deadline_ms,
                escalation_channel,
            } => format!(
                "cross — try Standard up to {}ms, escalate to Top Tier ({}) before {}ms total",
                standard_deadline_ms, escalation_channel, total_deadline_ms
            ),
            CredsMode::StandardOnly { total_deadline_ms } => {
                format!(
                    "standard-only — never escalate, fail after {}ms",
                    total_deadline_ms
                )
            }
            CredsMode::TopTierOnly {
                channel,
                total_deadline_ms,
            } => format!(
                "top-tier-only — pin to channel '{}', burns credits, deadline {}ms",
                channel, total_deadline_ms
            ),
            CredsMode::LocalOnly => "local-only — bypass mesh, use local engine".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_cross() {
        match CredsMode::default() {
            CredsMode::Cross { .. } => {}
            other => panic!("expected Cross default, got {other:?}"),
        }
    }

    #[test]
    fn cross_serialize_roundtrip() {
        let m = CredsMode::default();
        let s = serde_json::to_string(&m).unwrap();
        let back: CredsMode = serde_json::from_str(&s).unwrap();
        assert_eq!(back, m);
        assert!(s.contains("\"mode\":\"cross\""));
    }

    #[test]
    fn standard_only_serialize_shape() {
        let m = CredsMode::StandardOnly {
            total_deadline_ms: 12_000,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"mode\":\"standard_only\""));
        assert!(s.contains("\"total_deadline_ms\":12000"));
    }

    #[test]
    fn top_tier_only_serialize_shape() {
        let m = CredsMode::TopTierOnly {
            channel: "large_chat".into(),
            total_deadline_ms: 60_000,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"mode\":\"top_tier_only\""));
        assert!(s.contains("\"channel\":\"large_chat\""));
    }

    #[test]
    fn deserialize_with_only_required_fields_fills_defaults() {
        let s = r#"{"mode":"cross"}"#;
        let m: CredsMode = serde_json::from_str(s).unwrap();
        match m {
            CredsMode::Cross {
                standard_deadline_ms,
                total_deadline_ms,
                escalation_channel,
            } => {
                assert_eq!(standard_deadline_ms, 8_000);
                assert_eq!(total_deadline_ms, 30_000);
                assert_eq!(escalation_channel, "chat");
            }
            _ => panic!("not cross"),
        }
    }
}
