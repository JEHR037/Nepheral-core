// SPDX-License-Identifier: BUSL-1.1
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Standard,
    TopTier(TopTierChannel),
    Auto,
    Local,
    Dev(DevTemplate),
    Doc(DocTemplate),
    Review(ReviewTemplate),
    Research(ResearchTemplate),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopTierChannel {
    Chat,
    Large,
    Frontier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevTemplate {
    Refactor,
    AuditCode,
    GenTests,
    MigrateApi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocTemplate {
    Summarize,
    Extract,
    QueryCorpus,
    FindAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewTemplate {
    Security,
    Perf,
    Correctness,
    Contract,
    PrDiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchTemplate {
    OpenQuestion,
    ClaimCheck,
    CompareOptions,
}

pub fn resolve_model(name: &str) -> Option<Mode> {
    let n = name.trim();

    if matches!(n, "nepheral-auto" | "auto" | "gpt-4" | "gpt-3.5-turbo") {
        return Some(Mode::Auto);
    }
    if matches!(n, "nepheral-local") {
        return Some(Mode::Local);
    }
    if matches!(n, "nepheral-standard" | "qwen3:7b" | "qwen3-7b" | "default") {
        return Some(Mode::Standard);
    }

    match n {
        "nepheral-top-tier-chat" | "qwen3:72b" | "qwen3-72b" => {
            return Some(Mode::TopTier(TopTierChannel::Chat));
        }
        "nepheral-top-tier-large" => {
            return Some(Mode::TopTier(TopTierChannel::Large));
        }
        "nepheral-top-tier-frontier" => {
            return Some(Mode::TopTier(TopTierChannel::Frontier));
        }
        _ => {}
    }

    if let Some(template) = n.strip_prefix("nepheral-dev-") {
        let t = match template {
            "refactor" => DevTemplate::Refactor,
            "audit-code" => DevTemplate::AuditCode,
            "gen-tests" => DevTemplate::GenTests,
            "migrate-api" => DevTemplate::MigrateApi,
            _ => return None,
        };
        return Some(Mode::Dev(t));
    }
    if let Some(template) = n.strip_prefix("nepheral-doc-") {
        let t = match template {
            "summarize" => DocTemplate::Summarize,
            "extract" => DocTemplate::Extract,
            "query-corpus" => DocTemplate::QueryCorpus,
            "find-all" => DocTemplate::FindAll,
            _ => return None,
        };
        return Some(Mode::Doc(t));
    }
    if let Some(template) = n.strip_prefix("nepheral-review-") {
        let t = match template {
            "security" => ReviewTemplate::Security,
            "perf" => ReviewTemplate::Perf,
            "correctness" => ReviewTemplate::Correctness,
            "contract" => ReviewTemplate::Contract,
            "pr-diff" => ReviewTemplate::PrDiff,
            _ => return None,
        };
        return Some(Mode::Review(t));
    }
    if let Some(template) = n.strip_prefix("nepheral-research-") {
        let t = match template {
            "open-question" => ResearchTemplate::OpenQuestion,
            "claim-check" => ResearchTemplate::ClaimCheck,
            "compare-options" => ResearchTemplate::CompareOptions,
            _ => return None,
        };
        return Some(Mode::Research(t));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_aliases() {
        assert_eq!(resolve_model("nepheral-standard"), Some(Mode::Standard));
        assert_eq!(resolve_model("qwen3:7b"), Some(Mode::Standard));
        assert_eq!(resolve_model("default"), Some(Mode::Standard));
    }

    #[test]
    fn top_tier_channels() {
        assert_eq!(
            resolve_model("nepheral-top-tier-chat"),
            Some(Mode::TopTier(TopTierChannel::Chat))
        );
        assert_eq!(
            resolve_model("qwen3-72b"),
            Some(Mode::TopTier(TopTierChannel::Chat))
        );
        assert_eq!(
            resolve_model("nepheral-top-tier-frontier"),
            Some(Mode::TopTier(TopTierChannel::Frontier))
        );
    }

    #[test]
    fn dev_templates() {
        assert_eq!(
            resolve_model("nepheral-dev-refactor"),
            Some(Mode::Dev(DevTemplate::Refactor))
        );
        assert_eq!(
            resolve_model("nepheral-dev-audit-code"),
            Some(Mode::Dev(DevTemplate::AuditCode))
        );
    }

    #[test]
    fn auto_and_local_resolve() {
        assert_eq!(resolve_model("nepheral-auto"), Some(Mode::Auto));
        assert_eq!(resolve_model("auto"), Some(Mode::Auto));
        assert_eq!(resolve_model("gpt-4"), Some(Mode::Auto));
        assert_eq!(resolve_model("nepheral-local"), Some(Mode::Local));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(resolve_model("gpt-4-turbo"), None);
        assert_eq!(resolve_model("nepheral-dev-typo"), None);
        assert_eq!(resolve_model(""), None);
    }

    #[test]
    fn whitespace_tolerated() {
        assert_eq!(resolve_model("  nepheral-standard  "), Some(Mode::Standard));
    }
}
