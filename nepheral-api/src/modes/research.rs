// SPDX-License-Identifier: BUSL-1.1
use crate::dispatch::ResearchTemplate;

#[derive(Debug, Clone)]
pub struct ResearchAngle {
    pub name: &'static str,
    pub prompt_prefix: &'static str,
}

pub fn angles_for(template: ResearchTemplate) -> Vec<ResearchAngle> {
    match template {
        ResearchTemplate::OpenQuestion => vec![
            ResearchAngle {
                name: "formal",
                prompt_prefix: "You are answering a research question. Frame your response in \
                    rigorous, formal terms. Cite mechanisms, references, and concrete \
                    definitions where applicable. Be precise about what you know vs what you \
                    infer. Begin your answer with a one-line thesis.",
            },
            ResearchAngle {
                name: "pragmatic",
                prompt_prefix: "You are answering a research question. Frame your response in \
                    pragmatic, real-world terms. What does this mean for someone making a \
                    decision today? Skip theoretical ideals; focus on tradeoffs and concrete \
                    consequences. Begin with a one-line thesis.",
            },
            ResearchAngle {
                name: "skeptical",
                prompt_prefix: "You are answering a research question. Approach the question \
                    skeptically. What are the strongest reasons to doubt the obvious answer? \
                    What assumptions does the question itself embed? What counterexamples \
                    exist? Begin with a one-line thesis.",
            },
            ResearchAngle {
                name: "historical",
                prompt_prefix: "You are answering a research question. Frame your response \
                    historically. How has this question been answered before? What did past \
                    answers get right or wrong? What context changed? Begin with a one-line \
                    thesis.",
            },
        ],
        ResearchTemplate::ClaimCheck => vec![
            ResearchAngle {
                name: "verifier",
                prompt_prefix: "You are fact-checking the following claim. Identify each \
                    discrete factual sub-claim, then assess each one: TRUE, FALSE, UNVERIFIABLE, \
                    or PARTIALLY-TRUE. For each non-TRUE, give a one-sentence reason. End with \
                    an overall verdict.",
            },
            ResearchAngle {
                name: "refuter",
                prompt_prefix: "You are trying to REFUTE the following claim. Generate the \
                    strongest possible counter-arguments and counter-evidence. Default to \
                    refuted=true if uncertain — your job is to find holes, not to be neutral. \
                    End with a verdict on whether the claim survives.",
            },
            ResearchAngle {
                name: "defender",
                prompt_prefix: "You are trying to DEFEND the following claim. Generate the \
                    strongest possible supporting arguments and evidence. Default to defended=true \
                    if uncertain — your job is to find support. End with a verdict on the \
                    claim's strongest formulation.",
            },
        ],
        ResearchTemplate::CompareOptions => vec![
            ResearchAngle {
                name: "criteria",
                prompt_prefix: "You are comparing options. List the 4-6 most decision-relevant \
                    criteria. For each criterion, briefly state how each option performs. End \
                    with a recommendation conditional on what the decider values most.",
            },
            ResearchAngle {
                name: "risks",
                prompt_prefix: "You are comparing options. Focus exclusively on the failure modes \
                    of each option. What can go wrong? What is the cost of being wrong? End with \
                    the option whose failure mode is most survivable.",
            },
            ResearchAngle {
                name: "reversibility",
                prompt_prefix: "You are comparing options. Focus on reversibility: which option \
                    is easiest to abandon if it doesn't work out? Which locks in the most cost? \
                    End with the option that preserves the most optionality.",
            },
        ],
    }
}

pub fn assemble_research_output(
    template: ResearchTemplate,
    results: &[(String, String)],
) -> String {
    let template_name = template_name_human(template);
    let mut out = String::new();
    out.push_str(&format!(
        "# {} — ensemble research report\n\n",
        template_name
    ));
    out.push_str(&format!(
        "{} independent reasoning angles ran in parallel against the same question. \
         Each worker had no knowledge of the others' outputs — agreement across angles \
         is a positive signal; disagreement is information about what is contested.\n\n",
        results.len()
    ));

    out.push_str("## Per-angle responses\n\n");
    for (name, body) in results {
        out.push_str(&format!("### {name}\n\n{}\n\n", body.trim()));
    }

    let (agreements, divergences) = analyze_consensus(results);
    if !agreements.is_empty() {
        out.push_str("## Points of agreement\n\n");
        for line in &agreements {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    if !divergences.is_empty() {
        out.push_str("## Points of divergence\n\n");
        for line in &divergences {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(
        "_Generated by Nepheral's research orchestrator. Each angle ran on a different mesh \
         peer with no shared state._\n",
    );
    out
}

/// Heuristic consensus analyzer. Splits each response into sentences,
/// extracts key noun-like tokens (length >= 5, not stopword), counts
/// co-occurrence across responses. Returns:
///   agreements: terms that appear in >= 2/3 of responses
///   divergences: terms that appear in exactly 1 response (idiosyncratic claims)
fn analyze_consensus(results: &[(String, String)]) -> (Vec<String>, Vec<String>) {
    if results.len() < 2 {
        return (Vec::new(), Vec::new());
    }
    let n = results.len();
    let threshold = (n * 2).div_ceil(3);

    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (_, body) in results {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for word in body.split(|c: char| !c.is_alphanumeric() && c != '-') {
            let w = word.to_lowercase();
            if w.len() < 5 {
                continue;
            }
            if is_stopword(&w) {
                continue;
            }
            seen.insert(w);
        }
        for w in seen {
            *counts.entry(w).or_insert(0) += 1;
        }
    }

    let mut agreements: Vec<String> = counts
        .iter()
        .filter(|(_, &c)| c >= threshold)
        .map(|(w, c)| format!("\"{w}\" — surfaced in {c}/{n} angles"))
        .collect();
    let mut divergences: Vec<String> = counts
        .iter()
        .filter(|(_, &c)| c == 1)
        .filter(|(w, _)| w.len() >= 7)
        .take(8)
        .map(|(w, _)| format!("\"{w}\" — raised by only one angle, may be idiosyncratic"))
        .collect();
    agreements.sort();
    divergences.sort();
    agreements.truncate(12);
    (agreements, divergences)
}

fn is_stopword(w: &str) -> bool {
    matches!(
        w,
        "their"
            | "there"
            | "these"
            | "those"
            | "which"
            | "where"
            | "while"
            | "would"
            | "could"
            | "should"
            | "about"
            | "above"
            | "below"
            | "between"
            | "because"
            | "before"
            | "after"
            | "during"
            | "through"
            | "without"
            | "within"
            | "really"
            | "actually"
            | "however"
            | "therefore"
            | "moreover"
    )
}

fn template_name_human(t: ResearchTemplate) -> &'static str {
    match t {
        ResearchTemplate::OpenQuestion => "open-question",
        ResearchTemplate::ClaimCheck => "claim-check",
        ResearchTemplate::CompareOptions => "compare-options",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_template_has_at_least_three_angles() {
        for t in [
            ResearchTemplate::OpenQuestion,
            ResearchTemplate::ClaimCheck,
            ResearchTemplate::CompareOptions,
        ] {
            let angles = angles_for(t);
            assert!(
                angles.len() >= 3,
                "template {t:?} only {} angles",
                angles.len()
            );
        }
    }

    #[test]
    fn claim_check_includes_refuter_and_defender() {
        let names: Vec<_> = angles_for(ResearchTemplate::ClaimCheck)
            .iter()
            .map(|a| a.name)
            .collect();
        assert!(names.contains(&"refuter"));
        assert!(names.contains(&"defender"));
    }

    #[test]
    fn consensus_finds_agreements_across_majority() {
        let results = vec![
            (
                "a".to_string(),
                "Decentralization matters and quorum is critical.".to_string(),
            ),
            (
                "b".to_string(),
                "Decentralization is the main driver. Quorum protocols matter.".to_string(),
            ),
            (
                "c".to_string(),
                "Note that decentralization implies quorum constraints.".to_string(),
            ),
        ];
        let (agreements, _) = analyze_consensus(&results);
        let joined = agreements.join(" ");
        assert!(
            joined.contains("decentralization"),
            "expected 'decentralization' in agreements: {joined}"
        );
        assert!(
            joined.contains("quorum"),
            "expected 'quorum' in agreements: {joined}"
        );
    }

    #[test]
    fn consensus_reports_divergent_idiosyncratic_terms() {
        let results = vec![
            ("a".to_string(), "Things matter.".to_string()),
            (
                "b".to_string(),
                "Things matter and reversibility is important.".to_string(),
            ),
            (
                "c".to_string(),
                "Things matter and uniqueness is interesting.".to_string(),
            ),
        ];
        let (_, divergences) = analyze_consensus(&results);
        let joined = divergences.join(" ");
        // "reversibility" appears only in b; "uniqueness" only in c — both should surface
        assert!(joined.contains("reversibility") || joined.contains("uniqueness"));
    }

    #[test]
    fn assemble_includes_per_angle_sections() {
        let results = vec![
            ("formal".to_string(), "Formal answer text.".to_string()),
            (
                "pragmatic".to_string(),
                "Pragmatic answer text.".to_string(),
            ),
        ];
        let out = assemble_research_output(ResearchTemplate::OpenQuestion, &results);
        assert!(out.contains("### formal"));
        assert!(out.contains("### pragmatic"));
        assert!(out.contains("Per-angle responses"));
    }
}
