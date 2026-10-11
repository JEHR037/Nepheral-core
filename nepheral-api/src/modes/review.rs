// SPDX-License-Identifier: BUSL-1.1
use crate::dispatch::ReviewTemplate;

#[derive(Debug, Clone)]
pub struct ReviewLens {
    pub name: &'static str,
    pub prompt_prefix: &'static str,
}

pub fn lenses_for(template: ReviewTemplate) -> Vec<ReviewLens> {
    match template {
        ReviewTemplate::Security => vec![
            ReviewLens {
                name: "auth",
                prompt_prefix: "You are a security reviewer. Focus exclusively on authentication, \
                    authorization, session handling, and credential management. Report concrete \
                    findings with severity (Critical/High/Medium/Low) and a one-sentence \
                    explanation of the impact. Skip findings outside this scope.",
            },
            ReviewLens {
                name: "injection",
                prompt_prefix:
                    "You are a security reviewer. Focus exclusively on injection vectors: \
                    SQL, command, LDAP, XSS, template, deserialization, prototype pollution. \
                    Report concrete findings with severity (Critical/High/Medium/Low) and a \
                    one-sentence impact line. Skip findings outside this scope.",
            },
            ReviewLens {
                name: "secrets",
                prompt_prefix:
                    "You are a security reviewer. Focus exclusively on hard-coded secrets, \
                    credentials, API keys, private keys, weak crypto, insecure randomness, and \
                    sensitive data handling. Report concrete findings with severity \
                    (Critical/High/Medium/Low). Skip findings outside this scope.",
            },
            ReviewLens {
                name: "crypto",
                prompt_prefix: "You are a security reviewer. Focus exclusively on cryptographic \
                    correctness: cipher choice, key derivation, MAC vs encrypt-only, IV/nonce \
                    reuse, padding oracles, signature verification gaps. Report findings with \
                    severity. Skip findings outside this scope.",
            },
        ],
        ReviewTemplate::Perf => vec![
            ReviewLens {
                name: "complexity",
                prompt_prefix: "You are a performance reviewer. Focus exclusively on algorithmic \
                    complexity: O(N^2) loops where O(N) exists, redundant work in hot paths, \
                    accidental quadratic patterns, unnecessary allocations in tight loops. \
                    Report concrete findings with severity (Critical/High/Medium/Low).",
            },
            ReviewLens {
                name: "io",
                prompt_prefix:
                    "You are a performance reviewer. Focus exclusively on I/O patterns: \
                    sync I/O in async contexts, N+1 queries, missing batching, blocking calls in \
                    hot paths, unnecessary network round-trips. Report findings with severity.",
            },
            ReviewLens {
                name: "memory",
                prompt_prefix: "You are a performance reviewer. Focus exclusively on memory: \
                    unbounded growth, retained captures, leaks, excessive cloning, false sharing \
                    in concurrent contexts. Report findings with severity.",
            },
        ],
        ReviewTemplate::Correctness => vec![
            ReviewLens {
                name: "logic",
                prompt_prefix:
                    "You are a correctness reviewer. Focus exclusively on logic errors: \
                    off-by-one, wrong operator, inverted conditions, missing else branch, \
                    incorrect handling of edge values (empty, zero, max). Report concrete \
                    findings with severity (Critical/High/Medium/Low).",
            },
            ReviewLens {
                name: "concurrency",
                prompt_prefix: "You are a correctness reviewer. Focus exclusively on concurrency \
                    issues: data races, deadlocks, lock ordering violations, missing \
                    synchronization, TOCTOU. Report concrete findings with severity.",
            },
            ReviewLens {
                name: "error-handling",
                prompt_prefix:
                    "You are a correctness reviewer. Focus exclusively on error handling: \
                    silently swallowed errors, panics in production paths, missing rollback on \
                    partial failure, retry without backoff. Report findings with severity.",
            },
        ],
        ReviewTemplate::Contract => vec![
            ReviewLens {
                name: "obligations",
                prompt_prefix: "You are a contract reviewer. Focus exclusively on identifying \
                    each party's obligations, deadlines, and deliverables. Output a structured \
                    list grouped by party.",
            },
            ReviewLens {
                name: "risks",
                prompt_prefix:
                    "You are a contract reviewer. Focus exclusively on risk allocation, \
                    indemnity clauses, limits of liability, and termination conditions. Flag \
                    asymmetries and missing protections.",
            },
            ReviewLens {
                name: "ambiguity",
                prompt_prefix: "You are a contract reviewer. Focus exclusively on ambiguous, \
                    contradictory, or undefined terms. Report each ambiguity with a one-sentence \
                    explanation of why it is problematic.",
            },
        ],
        ReviewTemplate::PrDiff => vec![
            ReviewLens {
                name: "regression",
                prompt_prefix: "You are a PR reviewer. Focus exclusively on regression risk: \
                    behaviors silently changed, public API breakage, error-path divergence, \
                    test coverage of the changed lines. Report findings with severity.",
            },
            ReviewLens {
                name: "style",
                prompt_prefix: "You are a PR reviewer. Focus exclusively on code style and \
                    consistency: deviations from the surrounding patterns, naming convention \
                    violations, dead or unreachable code introduced. Report findings.",
            },
            ReviewLens {
                name: "tests",
                prompt_prefix: "You are a PR reviewer. Focus exclusively on test quality: \
                    new tests cover the new behavior, no flaky patterns, no over-mocked \
                    paths that mask real failures. Report findings.",
            },
        ],
    }
}

pub fn assemble_review_output(template_name: &str, lens_results: &[(String, String)]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# {} review — multi-lens audit\n\n",
        template_name
    ));
    out.push_str(&format!(
        "Aggregated findings from {} independent lenses, run in parallel across the Nepheral mesh. \
         Each lens was instructed to skip findings outside its scope; overlap between sections \
         is normal and indicates higher confidence.\n\n",
        lens_results.len()
    ));
    for (lens, body) in lens_results {
        out.push_str(&format!("## Lens: {}\n\n", lens));
        out.push_str(body.trim());
        out.push_str("\n\n");
    }
    out.push_str("---\n");
    out.push_str(
        "_Generated by Nepheral's review orchestrator. Each lens ran on an independent \
         mesh peer with no knowledge of the others._\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_template_has_at_least_three_lenses() {
        for t in [
            ReviewTemplate::Security,
            ReviewTemplate::Perf,
            ReviewTemplate::Correctness,
            ReviewTemplate::Contract,
            ReviewTemplate::PrDiff,
        ] {
            let lenses = lenses_for(t);
            assert!(
                lenses.len() >= 3,
                "template {t:?} produced only {} lens(es) — design contract is ≥3 for diversity",
                lenses.len()
            );
            for l in &lenses {
                assert!(!l.name.is_empty());
                assert!(!l.prompt_prefix.is_empty());
            }
        }
    }

    #[test]
    fn security_includes_crypto_lens() {
        let lenses = lenses_for(ReviewTemplate::Security);
        let names: Vec<_> = lenses.iter().map(|l| l.name).collect();
        assert!(names.contains(&"crypto"));
        assert!(names.contains(&"injection"));
        assert!(names.contains(&"auth"));
    }

    #[test]
    fn assemble_output_includes_each_lens_name() {
        let results = vec![
            ("auth".to_string(), "no findings".to_string()),
            ("injection".to_string(), "one Critical SQL".to_string()),
        ];
        let out = assemble_review_output("security", &results);
        assert!(out.contains("## Lens: auth"));
        assert!(out.contains("## Lens: injection"));
        assert!(out.contains("no findings"));
        assert!(out.contains("one Critical SQL"));
    }
}
