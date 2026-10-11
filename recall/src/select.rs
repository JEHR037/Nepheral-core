// SPDX-License-Identifier: BUSL-1.1
//! Which earlier messages a question carries, decided on the device.
//!
//! A question always takes the last few messages, the thread it continues.
//! Older messages go only when they are close enough in meaning to the
//! question, and then as few and as varied as possible (MMR). A request to
//! summarise the conversation takes instead one excerpt per topic: the
//! messages most representative of the whole, again kept varied. No language
//! model is involved, so this runs on a phone, and nothing is asked of the
//! network.
//!
//! What leaves is measured: the share of the older conversation's words it
//! carries. Past `Rules::confirm_share`, or if any fragment looks sensitive,
//! the person confirms before it is sent (docs/CONVERSATIONS.md).

/// How alike two things are, from 0 (unrelated) to 1 (the same). The vector
/// version compares embeddings; the lexical one, words. Indices are message
/// positions in the conversation.
pub trait Similarity {
    fn to_question(&self, message: usize) -> f32;
    fn between(&self, a: usize, b: usize) -> f32;
    /// For a long message, the words (`start..end`) closest to the question:
    /// what is sent of it instead of its beginning.
    fn passage(&self, _message: usize) -> Option<(usize, usize)> {
        None
    }
}

/// One message of the conversation, as the selection sees it.
#[derive(Debug, Clone)]
pub struct MessageInfo<'a> {
    pub text: &'a str,
    pub from_me: bool,
    /// Marked private by the person: never chosen, never sent.
    pub private: bool,
}

/// What the device knows about the question beyond similarity.
pub struct Signals<'a> {
    pub sim: &'a dyn Similarity,
    /// The question asks for a summary of the conversation.
    pub summary_request: bool,
    /// Per message: it touches a sensitive topic (health, money, identity…).
    pub sensitive: &'a [bool],
    /// The similarity an older message needs to count as relevant. Depends
    /// on the model (or on the lexical fallback).
    pub relevant_threshold: f32,
}

#[derive(Debug, Clone)]
pub struct Rules {
    /// The last messages always sent: the thread being continued.
    pub recent: usize,
    /// Older messages chosen for an ordinary question, at most.
    pub max_older: usize,
    /// Words of older context for an ordinary question, at most.
    pub older_word_budget: usize,
    /// One message is cut to this many words when sent as context.
    pub excerpt_words: usize,
    /// Excerpts in a summary, at most, each cut to `digest_excerpt_words`,
    /// and their words: short and many, to cover every topic.
    pub digest_max: usize,
    pub digest_excerpt_words: usize,
    pub digest_word_budget: usize,
    /// MMR's trade between relevance (1.0) and variety (0.0).
    pub mmr_lambda: f32,
    pub digest_lambda: f32,
    /// In a summary, the person's own messages count this much more: their
    /// questions name the topics, while answers, long and broad, are the
    /// most central to everything and would otherwise fill the digest.
    pub digest_own_weight: f32,
    /// Past this share of the older conversation's words, ask first.
    pub confirm_share: f32,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            recent: 2,
            max_older: 4,
            older_word_budget: 400,
            excerpt_words: 120,
            digest_max: 16,
            digest_excerpt_words: 40,
            digest_word_budget: 700,
            mmr_lambda: 0.7,
            digest_lambda: 0.5,
            digest_own_weight: 1.5,
            confirm_share: 0.30,
        }
    }
}

/// An older message chosen as context, cut to size.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    pub index: usize,
    pub from_me: bool,
    pub text: String,
    pub words: usize,
    pub sensitive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// The thread: the last messages, in order, sent as they are.
    pub recent: Vec<usize>,
    /// Older context, in conversation order.
    pub older: Vec<Fragment>,
    /// The older context is a per-topic digest, for a summary request.
    pub summary: bool,
    /// Words of older context sent, over the older conversation's words.
    pub older_share: f32,
    pub needs_confirmation: bool,
}

impl Plan {
    /// Leaves out the fragments the person removed when confirming, and
    /// recomputes what is left. `keep` lists message indices.
    pub fn keep_only(&mut self, keep: &[usize], older_words_total: usize) {
        self.older.retain(|f| keep.contains(&f.index));
        let sent: usize = self.older.iter().map(|f| f.words).sum();
        self.older_share = share(sent, older_words_total);
    }

    /// The text that goes in the question's `memory` field. The node that
    /// answers reads it as context; it never writes it back.
    pub fn memory_text(&self) -> Option<String> {
        if self.older.is_empty() {
            return None;
        }
        let mut out = if self.summary {
            String::from(
                "The conversation so far, one excerpt per topic, in order \
                 (chosen on the asker's device; use it for requests about the \
                 whole conversation):\n",
            )
        } else {
            String::from(
                "Earlier messages of this conversation related to the question \
                 (chosen on the asker's device):\n",
            )
        };
        for f in &self.older {
            out.push_str(if f.from_me {
                "- user: "
            } else {
                "- assistant: "
            });
            out.push_str(&f.text);
            out.push('\n');
        }
        Some(out)
    }
}

pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The first `n` words of `text`, marked when cut.
pub fn excerpt(text: &str, n: usize) -> (String, usize) {
    excerpt_from(text, None, n)
}

/// At most `n` words of `text`, from the start of `range` when given (a word
/// range), each cut end marked.
pub fn excerpt_from(text: &str, range: Option<(usize, usize)>, n: usize) -> (String, usize) {
    let words: Vec<&str> = text.split_whitespace().collect();
    let start = range.map_or(0, |r| r.0).min(words.len());
    let end = (start + n).min(words.len());
    let mut out = words[start..end].join(" ");
    if start > 0 {
        out = format!("… {out}");
    }
    if end < words.len() {
        out.push_str(" …");
    }
    (out, end - start)
}

fn share(sent: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        sent as f32 / total as f32
    }
}

/// The words of every message before the thread: what the share is
/// measured against. Private messages count: they are part of the
/// conversation, even though they never leave.
pub fn older_words(messages: &[MessageInfo], plan_recent: &[usize]) -> usize {
    let first_recent = plan_recent.first().copied().unwrap_or(messages.len());
    messages[..first_recent]
        .iter()
        .map(|m| word_count(m.text))
        .sum()
}

/// Maximal marginal relevance: picks, one at a time, the candidate that best
/// trades `relevance` against similarity to what is already picked, until
/// `max` items or `word_budget` words.
fn mmr(
    candidates: &[usize],
    relevance: &dyn Fn(usize) -> f32,
    sim: &dyn Similarity,
    lambda: f32,
    max: usize,
    word_budget: usize,
    words_of: &dyn Fn(usize) -> usize,
) -> Vec<usize> {
    let mut picked: Vec<usize> = Vec::new();
    let mut left: Vec<usize> = candidates.to_vec();
    let mut used = 0usize;
    while picked.len() < max && !left.is_empty() {
        let mut best: Option<(usize, f32)> = None;
        for (pos, &c) in left.iter().enumerate() {
            let redundancy = picked
                .iter()
                .map(|&p| sim.between(c, p))
                .fold(0.0f32, f32::max);
            let score = lambda * relevance(c) - (1.0 - lambda) * redundancy;
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((pos, score));
            }
        }
        let (pos, _) = best.expect("left is not empty");
        let c = left.remove(pos);
        let w = words_of(c);
        if used + w > word_budget && !picked.is_empty() {
            continue;
        }
        used += w;
        picked.push(c);
    }
    picked.sort_unstable();
    picked
}

pub fn plan(messages: &[MessageInfo], signals: &Signals, rules: &Rules) -> Plan {
    let eligible: Vec<usize> = (0..messages.len())
        .filter(|&i| !messages[i].private)
        .collect();
    let recent: Vec<usize> = eligible
        .iter()
        .rev()
        .take(rules.recent)
        .rev()
        .copied()
        .collect();
    let first_recent = recent.first().copied().unwrap_or(messages.len());
    let pool: Vec<usize> = eligible
        .iter()
        .copied()
        .filter(|&i| i < first_recent)
        .collect();
    let cut = |i: usize| {
        if signals.summary_request {
            excerpt(messages[i].text, rules.digest_excerpt_words)
        } else {
            excerpt_from(
                messages[i].text,
                signals.sim.passage(i),
                rules.excerpt_words,
            )
        }
    };
    let words_of = |i: usize| cut(i).1;

    let chosen = if signals.summary_request {
        // Central to the conversation, and varied: one excerpt per topic.
        let centrality = |i: usize| {
            if pool.len() < 2 {
                return 1.0;
            }
            let mean = pool
                .iter()
                .filter(|&&j| j != i)
                .map(|&j| signals.sim.between(i, j))
                .sum::<f32>()
                / (pool.len() - 1) as f32;
            if messages[i].from_me {
                mean * rules.digest_own_weight
            } else {
                mean
            }
        };
        mmr(
            &pool,
            &centrality,
            signals.sim,
            rules.digest_lambda,
            rules.digest_max,
            rules.digest_word_budget,
            &words_of,
        )
    } else {
        let relevant: Vec<usize> = pool
            .iter()
            .copied()
            .filter(|&i| signals.sim.to_question(i) >= signals.relevant_threshold)
            .collect();
        mmr(
            &relevant,
            &|i| signals.sim.to_question(i),
            signals.sim,
            rules.mmr_lambda,
            rules.max_older,
            rules.older_word_budget,
            &words_of,
        )
    };

    let older: Vec<Fragment> = chosen
        .into_iter()
        .map(|i| {
            let (text, words) = cut(i);
            Fragment {
                index: i,
                from_me: messages[i].from_me,
                text,
                words,
                sensitive: signals.sensitive.get(i).copied().unwrap_or(false),
            }
        })
        .collect();
    let sent: usize = older.iter().map(|f| f.words).sum();
    let older_share = share(sent, older_words(messages, &recent));
    let needs_confirmation = older_share > rules.confirm_share || older.iter().any(|f| f.sensitive);
    Plan {
        recent,
        older,
        summary: signals.summary_request,
        older_share,
        needs_confirmation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Similarity from a small table: `q[i]` to the question, `m[i][j]`
    /// between messages.
    struct Table {
        q: Vec<f32>,
        m: Vec<Vec<f32>>,
    }

    impl Similarity for Table {
        fn to_question(&self, i: usize) -> f32 {
            self.q[i]
        }
        fn between(&self, a: usize, b: usize) -> f32 {
            self.m[a][b]
        }
    }

    fn msgs<'a>(texts: &[&'a str]) -> Vec<MessageInfo<'a>> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| MessageInfo {
                text: t,
                from_me: i % 2 == 0,
                private: false,
            })
            .collect()
    }

    fn identity(n: usize) -> Vec<Vec<f32>> {
        (0..n)
            .map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
            .collect()
    }

    #[test]
    fn the_last_two_messages_always_go_and_unrelated_ones_stay() {
        let m = msgs(&["a b", "c d", "e f", "g h", "i j"]);
        let t = Table {
            q: vec![0.1, 0.1, 0.1, 0.1, 0.1],
            m: identity(5),
        };
        let s = Signals {
            sim: &t,
            summary_request: false,
            sensitive: &[false; 5],
            relevant_threshold: 0.5,
        };
        let p = plan(&m, &s, &Rules::default());
        assert_eq!(p.recent, vec![3, 4]);
        assert!(p.older.is_empty());
        assert!(!p.needs_confirmation);
        assert_eq!(p.memory_text(), None);
    }

    #[test]
    fn a_related_older_message_is_carried() {
        let m = msgs(&["bread recipe flour water", "x y", "z w", "u v", "s t"]);
        let t = Table {
            q: vec![0.9, 0.1, 0.1, 0.1, 0.1],
            m: identity(5),
        };
        let s = Signals {
            sim: &t,
            summary_request: false,
            sensitive: &[false; 5],
            relevant_threshold: 0.5,
        };
        let p = plan(&m, &s, &Rules::default());
        assert_eq!(p.older.iter().map(|f| f.index).collect::<Vec<_>>(), vec![0]);
        assert!(p.memory_text().unwrap().contains("bread recipe"));
    }

    #[test]
    fn near_duplicates_are_not_sent_twice() {
        let m = msgs(&["oven 220", "oven 220 degrees", "pasta", "a", "b", "c"]);
        let mut sim = identity(6);
        sim[0][1] = 0.98;
        sim[1][0] = 0.98;
        let t = Table {
            q: vec![0.9, 0.88, 0.6, 0.0, 0.0, 0.0],
            m: sim,
        };
        let s = Signals {
            sim: &t,
            summary_request: false,
            sensitive: &[false; 6],
            relevant_threshold: 0.5,
        };
        let p = plan(
            &m,
            &s,
            &Rules {
                max_older: 2,
                ..Rules::default()
            },
        );
        let chosen: Vec<usize> = p.older.iter().map(|f| f.index).collect();
        assert_eq!(chosen, vec![0, 2], "the second oven message adds nothing");
    }

    #[test]
    fn private_messages_never_go_even_when_relevant_or_recent() {
        let mut m = msgs(&["my password is x", "q", "r", "secret last"]);
        m[0].private = true;
        m[3].private = true;
        let t = Table {
            q: vec![1.0, 0.1, 0.1, 0.1],
            m: identity(4),
        };
        let s = Signals {
            sim: &t,
            summary_request: false,
            sensitive: &[false; 4],
            relevant_threshold: 0.5,
        };
        let p = plan(&m, &s, &Rules::default());
        assert_eq!(p.recent, vec![1, 2], "the thread skips the private message");
        assert!(p.older.is_empty());
    }

    #[test]
    fn more_than_thirty_percent_of_the_older_text_needs_confirmation() {
        // Four older messages of 10 words; one relevant: 25%, no question.
        let ten = "one two three four five six seven eight nine ten";
        let m = msgs(&[ten, ten, ten, ten, "a", "b"]);
        let t = Table {
            q: vec![0.9, 0.1, 0.1, 0.1, 0.0, 0.0],
            m: identity(6),
        };
        let s = Signals {
            sim: &t,
            summary_request: false,
            sensitive: &[false; 6],
            relevant_threshold: 0.5,
        };
        let p = plan(&m, &s, &Rules::default());
        assert!((p.older_share - 0.25).abs() < 1e-6);
        assert!(!p.needs_confirmation);
        // Two relevant: 50%, ask.
        let t = Table {
            q: vec![0.9, 0.8, 0.1, 0.1, 0.0, 0.0],
            m: identity(6),
        };
        let s = Signals {
            sim: &t,
            summary_request: false,
            sensitive: &[false; 6],
            relevant_threshold: 0.5,
        };
        let p = plan(&m, &s, &Rules::default());
        assert!(p.needs_confirmation);
    }

    #[test]
    fn a_sensitive_fragment_needs_confirmation_however_small() {
        let filler = "w ".repeat(200);
        let m = msgs(&["my diagnosis", &filler, &filler, &filler, "a", "b"]);
        let t = Table {
            q: vec![0.9, 0.0, 0.0, 0.0, 0.0, 0.0],
            m: identity(6),
        };
        let sensitive = [true, false, false, false, false, false];
        let s = Signals {
            sim: &t,
            summary_request: false,
            sensitive: &sensitive,
            relevant_threshold: 0.5,
        };
        let p = plan(&m, &s, &Rules::default());
        assert!(p.older_share < 0.30);
        assert!(p.needs_confirmation);
    }

    #[test]
    fn a_summary_takes_one_excerpt_per_topic() {
        // Two topics: messages 0-2 alike (cooking), 3-5 alike (travel).
        let m = msgs(&["c1", "c2", "c3", "t1", "t2", "t3", "last a", "last b"]);
        let mut sim = identity(8);
        for (a, b) in [(0, 1), (0, 2), (1, 2), (3, 4), (3, 5), (4, 5)] {
            sim[a][b] = 0.9;
            sim[b][a] = 0.9;
        }
        let t = Table {
            q: vec![0.0; 8],
            m: sim,
        };
        let s = Signals {
            sim: &t,
            summary_request: true,
            sensitive: &[false; 8],
            relevant_threshold: 0.5,
        };
        let p = plan(
            &m,
            &s,
            &Rules {
                digest_max: 2,
                ..Rules::default()
            },
        );
        assert!(p.summary);
        let picked: Vec<usize> = p.older.iter().map(|f| f.index).collect();
        assert_eq!(picked.len(), 2);
        assert!(
            picked[0] < 3 && picked[1] >= 3,
            "one per topic, not two of one: {picked:?}"
        );
        assert!(p
            .memory_text()
            .unwrap()
            .starts_with("The conversation so far"));
    }

    #[test]
    fn a_summary_prefers_the_persons_questions_to_broad_answers() {
        // Two topics, a question and its answer each. The answers (1, 3)
        // are a little more central; the questions (0, 2) name the topics.
        let m = msgs(&[
            "q cooking",
            "a cooking",
            "q travel",
            "a travel",
            "last a",
            "last b",
        ]);
        let mut sim = identity(6);
        for (a, b, v) in [
            (0, 1, 0.8),
            (2, 3, 0.8),
            (1, 3, 0.3),
            (0, 3, 0.3),
            (1, 2, 0.3),
            (0, 2, 0.1),
        ] {
            sim[a][b] = v;
            sim[b][a] = v;
        }
        let t = Table {
            q: vec![0.0; 6],
            m: sim,
        };
        let s = Signals {
            sim: &t,
            summary_request: true,
            sensitive: &[false; 6],
            relevant_threshold: 0.5,
        };
        let rules = Rules {
            digest_max: 2,
            ..Rules::default()
        };
        let picked =
            |r: &Rules| -> Vec<usize> { plan(&m, &s, r).older.iter().map(|f| f.index).collect() };
        assert_eq!(picked(&rules), vec![0, 2], "one question per topic");
        let unweighted = Rules {
            digest_own_weight: 1.0,
            ..rules
        };
        assert_eq!(
            picked(&unweighted),
            vec![1, 3],
            "without the weight, answers win"
        );
    }

    #[test]
    fn removing_fragments_when_confirming_lowers_the_share() {
        let ten = "one two three four five six seven eight nine ten";
        let m = msgs(&[ten, ten, ten, ten, "a", "b"]);
        let t = Table {
            q: vec![0.9, 0.8, 0.7, 0.1, 0.0, 0.0],
            m: identity(6),
        };
        let s = Signals {
            sim: &t,
            summary_request: false,
            sensitive: &[false; 6],
            relevant_threshold: 0.5,
        };
        let mut p = plan(&m, &s, &Rules::default());
        let total = older_words(&m, &p.recent);
        p.keep_only(&[0], total);
        assert_eq!(p.older.len(), 1);
        assert!((p.older_share - 0.25).abs() < 1e-6);
    }

    #[test]
    fn long_messages_are_cut() {
        let long = "w ".repeat(500);
        let (text, words) = excerpt(&long, 120);
        assert_eq!(words, 120);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn a_long_message_sends_its_relevant_passage() {
        let text: String = (0..300).map(|i| format!("w{i} ")).collect();
        let (cut, words) = excerpt_from(&text, Some((150, 270)), 120);
        assert_eq!(words, 120);
        assert!(cut.starts_with("… w150 "));
        assert!(cut.ends_with("w269 …"));
        // A passage at the very end is not marked as cut there.
        let (cut, _) = excerpt_from(&text, Some((250, 300)), 120);
        assert!(cut.starts_with("… w250") && cut.ends_with("w299"));
    }
}
