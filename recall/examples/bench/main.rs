// SPDX-License-Identifier: BUSL-1.1
//! Compares embedding models for conversation memory: retrieval quality in
//! several languages (same-language and across languages), detection of
//! "summarise the conversation" and of sensitive topics, and CPU speed.
//!
//! cargo run --release -p nepheral-recall --example bench -- <models-dir> [names…]

mod models;

use std::path::PathBuf;
use std::time::Instant;

use models::{Embedder as Candle, Family, Kind, ModelSpec, Pooling};
use nepheral_recall::embed::{cosine, Embedder as Lean};
use serde::Deserialize;

/// A candidate through candle, or the app's own encoder.
enum Embedder {
    Candle(Candle),
    Lean(Lean),
}

impl Embedder {
    fn embed(&self, texts: &[&str], kind: Kind) -> Result<Vec<Vec<f32>>, String> {
        match self {
            Embedder::Candle(e) => e.embed(texts, kind).map_err(|e| e.to_string()),
            Embedder::Lean(e) => e.embed(texts).map_err(|e| e.to_string()),
        }
    }
    fn dim(&self) -> usize {
        match self {
            Embedder::Candle(e) => e.dim(),
            Embedder::Lean(e) => e.dim(),
        }
    }
}

#[derive(Deserialize)]
struct Data {
    conversations: Vec<Conversation>,
    summary_intents: Labelled,
    sensitive: Labelled,
    /// Per conversation, questions that relate to none of it.
    #[serde(default)]
    unrelated: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct Conversation {
    name: String,
    turns: Vec<String>,
    probes: Vec<Probe>,
}

#[derive(Deserialize)]
struct Probe {
    q: String,
    gold: Vec<usize>,
}

#[derive(Deserialize)]
struct Labelled {
    positive: Vec<String>,
    negative: Vec<String>,
}

fn spec(family: Family, pooling: Pooling, q: &'static str, p: &'static str) -> ModelSpec {
    ModelSpec {
        family,
        pooling,
        query_prefix: q,
        passage_prefix: p,
        max_tokens: 256,
    }
}

/// Best accuracy over all thresholds, when "positive" means a score above it.
fn best_split(pos: &[f32], neg: &[f32]) -> (f32, f32) {
    let mut cuts: Vec<f32> = pos.iter().chain(neg).copied().collect();
    cuts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let total = (pos.len() + neg.len()) as f32;
    let mut best = (0.0, 0.0);
    for c in cuts {
        let ok = pos.iter().filter(|&&s| s >= c).count() + neg.iter().filter(|&&s| s < c).count();
        let acc = ok as f32 / total;
        if acc > best.0 {
            best = (acc, c);
        }
    }
    best
}

/// Each sentence scored by its nearest positive exemplar, leaving itself out.
fn exemplar_scores(e: &Embedder, set: &Labelled) -> (Vec<f32>, Vec<f32>) {
    let pos: Vec<&str> = set.positive.iter().map(String::as_str).collect();
    let neg: Vec<&str> = set.negative.iter().map(String::as_str).collect();
    let pv = e.embed(&pos, Kind::Query).unwrap();
    let nv = e.embed(&neg, Kind::Query).unwrap();
    let near = |v: &[f32], skip: Option<usize>| {
        pv.iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != skip)
            .map(|(_, p)| cosine(v, p))
            .fold(f32::MIN, f32::max)
    };
    let ps = pv
        .iter()
        .enumerate()
        .map(|(i, v)| near(v, Some(i)))
        .collect();
    let ns = nv.iter().map(|v| near(v, None)).collect();
    (ps, ns)
}

/// Scores of the best older messages for a question, and their median:
/// to see whether the best one stands out.
fn profile(
    r: &nepheral_recall::memory::Recall,
    turns: &[nepheral_recall::memory::Turn],
    q: &str,
    gold: &[usize],
) -> String {
    let qv = r.question(q).unwrap();
    let older = turns.len().saturating_sub(2);
    let mut s: Vec<(usize, f32)> = (0..older)
        .map(|i| {
            let best = turns[i]
                .vectors
                .unwrap()
                .passages
                .iter()
                .map(|p| cosine(&qv, &p.v))
                .fold(f32::MIN, f32::max);
            (i, best)
        })
        .collect();
    s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let median = s[s.len() / 2].1;
    let top: Vec<String> = s
        .iter()
        .take(3)
        .map(|(i, v)| format!("{}{i}:{v:.3}", if gold.contains(i) { "*" } else { "" }))
        .collect();
    format!(
        "top {} median {median:.3} lead {:.3}",
        top.join(" "),
        s[0].1 - median
    )
}

/// The crate's own detectors and `choose`, on sentences not in its examples.
fn check_recall(root: &std::path::Path, data: &Data) {
    use nepheral_recall::memory::{self, Recall, Turn};
    use nepheral_recall::select::Rules;
    let r = Recall::load_packed(&root.join("nai-memory-v1.safetensors")).unwrap();
    let show = |name: &str, set: &Labelled, score: &dyn Fn(&str) -> f32, cut: f32| {
        let mut pos: Vec<(f32, &str)> = set
            .positive
            .iter()
            .map(|t| (score(t), t.as_str()))
            .collect();
        let mut neg: Vec<(f32, &str)> = set
            .negative
            .iter()
            .map(|t| (score(t), t.as_str()))
            .collect();
        pos.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        neg.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let ps: Vec<f32> = pos.iter().map(|p| p.0).collect();
        let ns: Vec<f32> = neg.iter().map(|p| p.0).collect();
        let (acc, at) = best_split(&ps, &ns);
        let missed = pos.iter().filter(|p| p.0 < cut).count();
        let false_alarms = neg.iter().filter(|p| p.0 >= cut).count();
        println!("{name}: best {acc:.2} @ {at:.2}; at {cut:.2}: {missed} missed, {false_alarms} false alarms");
        println!("   lowest positives: {:?}", &pos[..3.min(pos.len())]);
        println!("   highest negatives: {:?}", &neg[..3.min(neg.len())]);
    };
    show(
        "summary request",
        &data.summary_intents,
        &|t| r.summary_score(&r.question(t).unwrap()),
        memory::SUMMARY,
    );
    show(
        "sensitive",
        &data.sensitive,
        &|t| r.sensitive_score(&r.question(t).unwrap()),
        memory::SENSITIVE_AT,
    );

    // End to end on every conversation: the gold among what is sent, and
    // nothing sent for unrelated questions.
    let (mut found, mut probes, mut sent, mut unrelated_sent, mut unrelated) = (0, 0, 0, 0, 0);
    let mut confirmations = 0;
    for c in &data.conversations {
        let texts: Vec<&str> = c.turns.iter().map(String::as_str).collect();
        let vectors = r.messages(&texts).unwrap();
        let turns: Vec<Turn> = texts
            .iter()
            .zip(&vectors)
            .enumerate()
            .map(|(i, (t, v))| Turn {
                text: t,
                from_me: i % 2 == 0,
                private: false,
                vectors: Some(v),
            })
            .collect();
        for p in &c.probes {
            let plan = memory::choose(&turns, &p.q, Some(&r), &Rules::default()).unwrap();
            probes += 1;
            sent += plan.older.len();
            confirmations += plan.needs_confirmation as usize;
            if plan.older.iter().any(|f| p.gold.contains(&f.index)) {
                found += 1;
            } else {
                println!(
                    "   missed in {}: {:?} -> {:?} {}",
                    c.name,
                    p.q,
                    plan.older.iter().map(|f| f.index).collect::<Vec<_>>(),
                    profile(&r, &turns, &p.q, &p.gold)
                );
            }
        }
        for q in data.unrelated.get(&c.name).into_iter().flatten() {
            let plan = memory::choose(&turns, q, Some(&r), &Rules::default()).unwrap();
            unrelated += 1;
            if !plan.older.is_empty() {
                unrelated_sent += 1;
                println!(
                    "   unrelated {:?} carried {:?}",
                    q,
                    plan.older.iter().map(|f| f.index).collect::<Vec<_>>()
                );
            }
            println!("   unrelated {:?}: {}", q, profile(&r, &turns, q, &[]));
        }
        let plan = memory::choose(
            &turns,
            "Hazme un resumen de toda la conversación",
            Some(&r),
            &Rules::default(),
        )
        .unwrap();
        println!(
            "   summary of {}: {} excerpts {:?}, share {:.2}, confirm {}",
            c.name,
            plan.older.len(),
            plan.older.iter().map(|f| f.index).collect::<Vec<_>>(),
            plan.older_share,
            plan.needs_confirmation
        );
    }
    println!(
        "choose: related context found for {found}/{probes} questions ({:.1} messages each, {confirmations} to confirm); unrelated questions carrying context: {unrelated_sent}/{unrelated}",
        sent as f32 / probes as f32
    );
}

fn main() {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: bench <models-dir> [names…]"),
    );
    let data: Data = serde_json::from_str(include_str!("data.json")).unwrap();
    let models = [
        (
            "e5-small",
            "e5-small",
            spec(Family::Bert, Pooling::Mean, "query: ", "passage: "),
        ),
        (
            "e5-small-sym",
            "e5-small",
            spec(Family::Bert, Pooling::Mean, "query: ", "query: "),
        ),
        (
            "minilm-l12",
            "minilm-l12",
            spec(Family::Bert, Pooling::Mean, "", ""),
        ),
        (
            "granite-cls",
            "granite-107m",
            spec(Family::XlmRoberta, Pooling::Cls, "", ""),
        ),
        (
            "granite-mean",
            "granite-107m",
            spec(Family::XlmRoberta, Pooling::Mean, "", ""),
        ),
        (
            "potion-128m",
            "potion-128m",
            spec(Family::Static, Pooling::Mean, "", ""),
        ),
        (
            "granite-lean",
            "granite-107m",
            spec(Family::XlmRoberta, Pooling::Cls, "", ""),
        ),
        (
            "granite-packed",
            "granite-107m",
            spec(Family::XlmRoberta, Pooling::Cls, "", ""),
        ),
    ];
    let only: Vec<String> = std::env::args().skip(2).collect();
    if only.iter().any(|o| o == "recall") {
        check_recall(&root, &data);
        return;
    }
    for (label, dir, model_spec) in models {
        if !only.is_empty() && !only.iter().any(|o| o == label) {
            continue;
        }
        let t = Instant::now();
        let loaded = if label == "granite-lean" {
            Lean::load(&root.join(dir))
                .map(Embedder::Lean)
                .map_err(|e| e.to_string())
        } else if label == "granite-packed" {
            Lean::load_packed(&root.join("nai-memory-v1.safetensors"))
                .map(Embedder::Lean)
                .map_err(|e| e.to_string())
        } else {
            Candle::load(&root.join(dir), model_spec)
                .map(Embedder::Candle)
                .map_err(|e| e.to_string())
        };
        let e = match loaded {
            Ok(e) => e,
            Err(err) => {
                println!("{label}: load failed: {err}");
                continue;
            }
        };
        let load_ms = t.elapsed().as_millis();

        let (mut probes, mut mrr, mut recall4, mut top1) = (0usize, 0f32, 0f32, 0usize);
        let mut gold_best = Vec::new();
        let mut other_best = Vec::new();
        let mut per_conv = Vec::new();
        // Every candidate's score, split by whether it is relevant; and the
        // best score of questions about nothing in the conversation.
        let (mut pair_gold, mut pair_other, mut unrelated_best) =
            (Vec::new(), Vec::new(), Vec::new());
        let mut embed_ms = 0u128;
        let mut embedded = 0usize;
        for c in &data.conversations {
            let turns: Vec<&str> = c.turns.iter().map(String::as_str).collect();
            let t = Instant::now();
            let tv = e.embed(&turns, Kind::Passage).unwrap();
            embed_ms += t.elapsed().as_millis();
            embedded += turns.len();
            let candidates = turns.len().saturating_sub(2);
            let mut conv_mrr = 0f32;
            for p in &c.probes {
                let qv = &e.embed(&[p.q.as_str()], Kind::Query).unwrap()[0];
                let mut ranked: Vec<(usize, f32)> =
                    (0..candidates).map(|i| (i, cosine(qv, &tv[i]))).collect();
                ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                for (i, s) in &ranked {
                    if p.gold.contains(i) {
                        pair_gold.push(*s);
                    } else {
                        pair_other.push(*s);
                    }
                }
                let first = ranked.iter().position(|(i, _)| p.gold.contains(i)).unwrap();
                let rr = 1.0 / (first as f32 + 1.0);
                mrr += rr;
                conv_mrr += rr;
                if first == 0 {
                    top1 += 1;
                }
                let want = p.gold.iter().filter(|&&g| g < candidates).count().min(4);
                let hit = ranked
                    .iter()
                    .take(4)
                    .filter(|(i, _)| p.gold.contains(i))
                    .count();
                recall4 += hit as f32 / want as f32;
                gold_best.push(ranked[first].1);
                other_best.push(
                    ranked
                        .iter()
                        .find(|(i, _)| !p.gold.contains(i))
                        .map(|r| r.1)
                        .unwrap_or(0.0),
                );
                probes += 1;
            }
            for q in data.unrelated.get(&c.name).into_iter().flatten() {
                let qv = &e.embed(&[q.as_str()], Kind::Query).unwrap()[0];
                unrelated_best.push(
                    (0..candidates)
                        .map(|i| cosine(qv, &tv[i]))
                        .fold(f32::MIN, f32::max),
                );
            }
            per_conv.push(format!(
                "{}={:.2}",
                c.name,
                conv_mrr / c.probes.len() as f32
            ));
        }
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let (s_pos, s_neg) = exemplar_scores(&e, &data.summary_intents);
        let (summary_acc, summary_cut) = best_split(&s_pos, &s_neg);
        let (x_pos, x_neg) = exemplar_scores(&e, &data.sensitive);
        let (pair_acc, pair_cut) = best_split(&pair_gold, &pair_other);
        let (rel_acc, rel_cut) = best_split(&gold_best, &unrelated_best);
        let (sens_acc, sens_cut) = best_split(&x_pos, &x_neg);

        // Speed on one typical message and on a long answer.
        let short = "¿Qué barrio de Lisboa me recomendaste para escuchar fado por la noche?";
        let long = data.conversations[0].turns[9].repeat(4);
        let t = Instant::now();
        for _ in 0..10 {
            e.embed(&[short], Kind::Query).unwrap();
        }
        let short_ms = t.elapsed().as_secs_f32() * 100.0;
        let t = Instant::now();
        for _ in 0..3 {
            e.embed(&[long.as_str()], Kind::Passage).unwrap();
        }
        let long_ms = t.elapsed().as_secs_f32() * 1000.0 / 3.0;

        println!(
            "{label:13} dim {:3} load {load_ms:5} ms | MRR {:.3} top1 {:2}/{probes} R@4 {:.3} | gold {:.3} vs other {:.3} (gap {:.3}) | summary acc {:.2} @ {:.2} | sensitive acc {:.2} @ {:.2} | {:.1} ms/msg batch, {short_ms:.1} ms short, {long_ms:.0} ms long",
            e.dim(),
            mrr / probes as f32,
            top1,
            recall4 / probes as f32,
            mean(&gold_best),
            mean(&other_best),
            mean(&gold_best) - mean(&other_best),
            summary_acc,
            summary_cut,
            sens_acc,
            sens_cut,
            embed_ms as f32 / embedded as f32,
        );
        println!("              {}", per_conv.join("  "));
        if label.starts_with("granite-") && label != "granite-cls" && label != "granite-mean" {
            // Same vectors as candle's XLM-RoBERTa, or the encoder is wrong.
            let reference = Candle::load(
                &root.join(dir),
                spec(Family::XlmRoberta, Pooling::Cls, "", ""),
            )
            .unwrap();
            let texts: Vec<&str> = data
                .conversations
                .iter()
                .flat_map(|c| c.turns.iter().map(String::as_str))
                .collect();
            let a = e.embed(&texts, Kind::Passage).unwrap();
            let b = reference.embed(&texts, Kind::Passage).unwrap();
            let worst = a
                .iter()
                .zip(&b)
                .map(|(x, y)| cosine(x, y))
                .fold(f32::MAX, f32::min);
            println!(
                "              lean vs candle: worst cosine over {} texts {worst:.6}",
                texts.len()
            );
        }
        println!(
            "              one threshold: relevant vs not {pair_acc:.3} @ {pair_cut:.2}; has-history vs unrelated question {rel_acc:.3} @ {rel_cut:.2} (unrelated max {:.3})",
            unrelated_best.iter().copied().fold(f32::MIN, f32::max)
        );
    }
}
