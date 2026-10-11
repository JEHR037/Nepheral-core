// SPDX-License-Identifier: BUSL-1.1
//! The conversation side: vectors for each message, kept with it, and the
//! choice of context for a new question.
//!
//! With the model on the device, "related" is measured in meaning, in any
//! language. Until it is downloaded, or where it cannot load, a lexical
//! measure takes its place: weaker, but the rules (`select`) and what leaves
//! the device are the same.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::embed::{cosine, EmbedError, Embedder};
use crate::exemplars::{NOT_SENSITIVE, SENSITIVE, SUMMARY_REQUESTS};
use crate::select::{plan, MessageInfo, Plan, Rules, Signals, Similarity};

/// Long messages are embedded in passages of this many words (~200 tokens in
/// Spanish, under the model's limit). Equal to `Rules::excerpt_words`, so a
/// chosen passage is sent whole.
pub const PASSAGE_WORDS: usize = 120;
/// The version of the vectors kept with messages: a new model makes old
/// ones useless, and they are computed again.
/// 2: `sensitive` is the model's verdict alone, no longer mixed with the
/// word list.
/// 3: the verdict also weighs the examples of what is not sensitive.
pub const VECTORS_VERSION: u32 = 3;

/// Thresholds for Granite Embedding 107M, measured with `examples/bench`.
/// An older message counts as related to the question from here.
pub const RELEVANT: f32 = 0.65;
/// A question that refers back to the conversation ("what you told me
/// about…") needs less to count something as related.
pub const RELEVANT_REFERRED: f32 = 0.57;
/// A question this close to a summary request is one.
pub const SUMMARY: f32 = 0.77;
/// A passage this close to a sensitive example is sensitive. Set low: a
/// false alarm costs a confirmation, a miss sends something unnoticed.
pub const SENSITIVE_AT: f32 = 0.67;
/// ...and at least this much closer to it than to the nearest everyday
/// example (`NOT_SENSITIVE`): a dog's cancer or technical debt is close to
/// the sensitive examples too, and closer still to the everyday ones.
pub const SENSITIVE_MARGIN: f32 = 0.03;
/// The lexical fallback's thresholds.
pub const RELEVANT_LEXICAL: f32 = 0.2;
pub const RELEVANT_LEXICAL_REFERRED: f32 = 0.12;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Passage {
    /// Word range in the message.
    pub start: usize,
    pub end: usize,
    pub v: Vec<f32>,
}

/// What the device keeps of a message to compare it: computed once, sealed
/// with the conversation when it is saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageVectors {
    pub version: u32,
    pub passages: Vec<Passage>,
    /// The passages' mean, unit length: the message as a whole.
    pub mean: Vec<f32>,
    pub sensitive: bool,
}

/// The model and the examples it compares with.
pub struct Recall {
    embedder: Embedder,
    summary: Vec<Vec<f32>>,
    sensitive: Vec<Vec<f32>>,
    not_sensitive: Vec<Vec<f32>>,
}

fn normalized(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

fn nearest(v: &[f32], set: &[Vec<f32>]) -> f32 {
    set.iter().map(|e| cosine(v, e)).fold(f32::MIN, f32::max)
}

/// Word ranges of at most `PASSAGE_WORDS`, covering the text.
fn passage_ranges(words: usize) -> Vec<(usize, usize)> {
    if words == 0 {
        return vec![(0, 0)];
    }
    (0..words)
        .step_by(PASSAGE_WORDS)
        .map(|s| (s, (s + PASSAGE_WORDS).min(words)))
        .collect()
}

impl Recall {
    pub fn new(embedder: Embedder) -> Result<Self, EmbedError> {
        let summary = embedder.embed(SUMMARY_REQUESTS)?;
        let sensitive = embedder.embed(SENSITIVE)?;
        let not_sensitive = embedder.embed(NOT_SENSITIVE)?;
        Ok(Self {
            embedder,
            summary,
            sensitive,
            not_sensitive,
        })
    }

    /// The file the app downloads (`embed::Embedder::load_packed`).
    pub fn load_packed(path: &std::path::Path) -> Result<Self, EmbedError> {
        Self::new(Embedder::load_packed(path)?)
    }

    pub fn messages(&self, texts: &[&str]) -> Result<Vec<MessageVectors>, EmbedError> {
        let mut spans = Vec::new();
        let mut owners = Vec::new();
        let split: Vec<Vec<&str>> = texts
            .iter()
            .map(|t| t.split_whitespace().collect())
            .collect();
        for (m, words) in split.iter().enumerate() {
            for (s, e) in passage_ranges(words.len()) {
                spans.push(words[s..e].join(" "));
                owners.push((m, s, e));
            }
        }
        let refs: Vec<&str> = spans.iter().map(String::as_str).collect();
        let vectors = self.embedder.embed(&refs)?;
        let mut out: Vec<MessageVectors> = texts
            .iter()
            .map(|_| MessageVectors {
                version: VECTORS_VERSION,
                passages: Vec::new(),
                mean: Vec::new(),
                sensitive: false,
            })
            .collect();
        for ((m, start, end), v) in owners.into_iter().zip(vectors) {
            let mv = &mut out[m];
            mv.sensitive |= self.is_sensitive(&v);
            mv.passages.push(Passage { start, end, v });
        }
        for mv in out.iter_mut() {
            let dim = self.embedder.dim();
            let mut sum = vec![0f32; dim];
            for p in &mv.passages {
                sum.iter_mut().zip(&p.v).for_each(|(s, x)| *s += x);
            }
            mv.mean = normalized(sum);
        }
        Ok(out)
    }

    pub fn question(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(self.embedder.embed(&[text])?.remove(0))
    }

    pub fn summary_score(&self, question: &[f32]) -> f32 {
        nearest(question, &self.summary)
    }

    pub fn sensitive_score(&self, v: &[f32]) -> f32 {
        nearest(v, &self.sensitive)
    }

    /// Close to a sensitive example, and clearly closer to it than to any
    /// everyday one.
    pub fn is_sensitive(&self, v: &[f32]) -> bool {
        let s = nearest(v, &self.sensitive);
        s >= SENSITIVE_AT && s - nearest(v, &self.not_sensitive) >= SENSITIVE_MARGIN
    }
}

/// One message as the conversation keeps it.
#[derive(Debug, Clone)]
pub struct Turn<'a> {
    pub text: &'a str,
    pub from_me: bool,
    pub private: bool,
    /// Present once computed with the current model.
    pub vectors: Option<&'a MessageVectors>,
}

struct VectorSim<'a> {
    question: Vec<f32>,
    turns: &'a [Turn<'a>],
}

impl VectorSim<'_> {
    fn best(&self, message: usize) -> Option<(f32, usize, usize)> {
        self.turns[message]
            .vectors?
            .passages
            .iter()
            .map(|p| (cosine(&self.question, &p.v), p.start, p.end))
            .reduce(|a, b| if b.0 > a.0 { b } else { a })
    }
}

impl Similarity for VectorSim<'_> {
    fn to_question(&self, message: usize) -> f32 {
        self.best(message).map_or(0.0, |b| b.0)
    }
    fn between(&self, a: usize, b: usize) -> f32 {
        match (self.turns[a].vectors, self.turns[b].vectors) {
            (Some(x), Some(y)) => cosine(&x.mean, &y.mean),
            _ => 0.0,
        }
    }
    fn passage(&self, message: usize) -> Option<(usize, usize)> {
        let v = self.turns[message].vectors?;
        if v.passages.len() < 2 {
            return None;
        }
        self.best(message).map(|b| (b.1, b.2))
    }
}

/// Lowercase, without accents, split into words of three letters or more,
/// minus the commonest words of the main languages.
fn terms(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "que", "los", "las", "una", "uno", "por", "para", "con", "del", "como", "pero", "mas",
        "esto", "esta", "este", "eso", "ese", "esa", "hay", "muy", "the", "and", "for", "you",
        "with", "that", "this", "what", "how", "are", "was", "have", "from", "your", "can", "not",
        "nao", "uma", "com", "voce", "les", "des", "est", "une", "pour", "dans", "qui", "und",
        "der", "die", "das", "ist", "nicht", "mit", "ein", "eine", "che", "per", "non", "sono",
        "me", "dime", "dijiste", "tell", "said",
    ];
    let folded: String = text
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    let mut out: Vec<String> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(w))
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn overlap(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.iter().filter(|w| b.binary_search(w).is_ok()).count();
    shared as f32 / ((a.len() * b.len()) as f32).sqrt()
}

struct LexicalSim {
    question: Vec<String>,
    turns: Vec<Vec<String>>,
}

impl Similarity for LexicalSim {
    fn to_question(&self, message: usize) -> f32 {
        overlap(&self.question, &self.turns[message])
    }
    fn between(&self, a: usize, b: usize) -> f32 {
        overlap(&self.turns[a], &self.turns[b])
    }
}

/// Without the model: phrases that ask about the whole conversation.
pub fn lexical_summary_request(text: &str) -> bool {
    let t = terms(text).join(" ");
    let has = |w: &str| t.split(' ').any(|x| x.starts_with(w));
    let about_chat = has("conversac")
        || has("conversa")
        || has("conversation")
        || has("chat")
        || has("discus")
        || has("hablad")
        || has("hablamos")
        || has("talked")
        || has("gesprach")
        || has("conversazione");
    let summarise =
        has("resum") || has("summar") || has("recap") || has("zusammen") || has("riassum");
    (summarise && about_chat) || has("recapitul")
}

/// The question points back at something said earlier: "you told me",
/// "lo del…", "again", or a Spanish imperfect ("¿qué llevaba?").
pub fn refers_back(text: &str) -> bool {
    const PHRASES: &[&str] = &[
        "dijiste",
        "mencionaste",
        "comentaste",
        "recomendaste",
        "explicaste",
        "me diste",
        "lo de",
        "lo del",
        "recuerdame",
        "como era",
        "cual era",
        "que era",
        "de nuevo",
        "otra vez",
        "de antes",
        "you said",
        "you told",
        "you mentioned",
        "you recommended",
        "you suggested",
        "you gave",
        "remind me",
        "again",
        "earlier",
        "voce disse",
        "me disse",
        "voce falou",
        "lembra",
        "de novo",
        "tu m'as dit",
        "tu as dit",
        "encore",
        "rappelle",
        "du hast gesagt",
        "du sagtest",
        "nochmal",
        "noch einmal",
        "erinnere",
        "hai detto",
        "ricordami",
        "di nuovo",
    ];
    let folded: String = text
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    let words: Vec<&str> = folded
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    let joined = format!(" {} ", words.join(" "));
    PHRASES.iter().any(|p| joined.contains(&format!(" {p} ")))
        || words.iter().any(|w| {
            w.chars().count() > 4
                && (w.ends_with("aba") || w.ends_with("abas") || w.ends_with("aban"))
        })
}

/// Without the model: what people write when they speak of their own
/// health, money trouble, the law or private life.
///
/// Words with an everyday meaning are left to the model, which compares
/// whole passages with first-person examples: "sin embargo" is "however",
/// "deuda técnica" is code, "diagnosticar un error" is debugging, and NAI's
/// own answers use "terapia" or "violencia" all the time.
pub fn lexical_sensitive(text: &str) -> bool {
    const STEMS: &[&str] = &[
        "quimio",
        "chemo",
        "suicid",
        "autolesi",
        "antidepres",
        "psiquiatr",
        "psychiatr",
        "indocument",
        "undocument",
        "maltrat",
    ];
    const WORDS: &[&str] = &["vih", "hiv", "sida"];
    const PHRASES: &[&str] = &[
        // Health.
        "me diagnosticaron",
        "me han diagnosticado",
        "fui diagnosticado",
        "fui diagnosticada",
        "i was diagnosed",
        "i ve been diagnosed",
        "i have been diagnosed",
        "tengo cancer",
        "i have cancer",
        "estoy embarazada",
        "i m pregnant",
        "i am pregnant",
        "estou gravida",
        "self harm",
        "me corto",
        "cutting myself",
        "me quiero morir",
        "kill myself",
        // Credentials.
        "mi contrasena",
        "my password",
        "minha senha",
        "mein passwort",
        "mon mot de passe",
        // Money and the law.
        "tengo deudas",
        "estoy en quiebra",
        "i m bankrupt",
        "i am bankrupt",
        "sin papeles",
        "antecedentes penales",
        "criminal record",
        // Private life.
        "me pega",
        "hits me",
        "soy gay",
        "soy lesbiana",
        "soy bisexual",
        "soy trans",
        "i m gay",
        "i am gay",
        "i m trans",
        "i am trans",
        "me estoy divorciando",
        "getting divorced",
        "cheating on my",
        "le soy infiel",
    ];
    if terms(text)
        .iter()
        .any(|w| WORDS.contains(&w.as_str()) || STEMS.iter().any(|s| w.starts_with(s)))
    {
        return true;
    }
    let folded: String = text
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase();
    let spaced = format!(
        " {} ",
        folded
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    );
    PHRASES.iter().any(|p| spaced.contains(&format!(" {p} ")))
}

/// Whether a message needs the person's OK before it leaves as context.
///
/// Only their own messages count: NAI's answers repeat topics and set off
/// nearly every question. With the model on the device, its verdict alone
/// decides, in any language, by meaning; the word list is only for until
/// the model is there.
fn sensitive_turn(t: &Turn) -> bool {
    t.from_me
        && t.vectors
            .map_or_else(|| lexical_sensitive(t.text), |v| v.sensitive)
}

/// What a question carries, chosen on the device. `recall` is the model when
/// loaded; the turns' vectors must then be present for every message that
/// is not private (see `Recall::messages`), or the lexical measure is used.
pub fn choose(
    turns: &[Turn],
    question: &str,
    recall: Option<&Recall>,
    rules: &Rules,
) -> Result<Plan, EmbedError> {
    let infos: Vec<MessageInfo> = turns
        .iter()
        .map(|t| MessageInfo {
            text: t.text,
            from_me: t.from_me,
            private: t.private,
        })
        .collect();
    let vectors_ready = turns
        .iter()
        .all(|t| t.private || t.vectors.is_some_and(|v| v.version == VECTORS_VERSION));
    let sensitive: Vec<bool> = turns.iter().map(sensitive_turn).collect();
    let lexical_summary = lexical_summary_request(question);
    let referred = refers_back(question);
    match recall {
        Some(r) if vectors_ready => {
            let q = r.question(question)?;
            let summary_request = lexical_summary || r.summary_score(&q) >= SUMMARY;
            let sim = VectorSim { question: q, turns };
            let signals = Signals {
                sim: &sim,
                summary_request,
                sensitive: &sensitive,
                relevant_threshold: if referred {
                    RELEVANT_REFERRED
                } else {
                    RELEVANT
                },
            };
            Ok(plan(&infos, &signals, rules))
        }
        _ => {
            let sim = LexicalSim {
                question: terms(question),
                turns: turns.iter().map(|t| terms(t.text)).collect(),
            };
            let signals = Signals {
                sim: &sim,
                summary_request: lexical_summary,
                sensitive: &sensitive,
                relevant_threshold: if referred {
                    RELEVANT_LEXICAL_REFERRED
                } else {
                    RELEVANT_LEXICAL
                },
            };
            Ok(plan(&infos, &signals, rules))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held-out sentences, none of them among the examples. Run with the
    /// model: `NEPHERAL_MEMORY_MODEL=<path to nai-memory-v1.safetensors>
    /// cargo test -p nepheral-recall -- --ignored`.
    #[test]
    #[ignore = "needs the memory model file (NEPHERAL_MEMORY_MODEL)"]
    fn the_model_tells_personal_topics_from_everyday_ones() {
        let path = std::env::var("NEPHERAL_MEMORY_MODEL").expect("NEPHERAL_MEMORY_MODEL");
        let r = Recall::load_packed(std::path::Path::new(&path)).unwrap();
        let verdict = |t: &str| r.is_sensitive(&r.question(t).unwrap());
        for t in [
            "Me diagnosticaron diabetes el mes pasado",
            "I've been depressed for months and can't sleep",
            "Tengo una deuda de 20.000 euros con el banco",
            "My password is hunter2",
            "Me operan del corazón la semana que viene",
            "My therapist says I have PTSD",
        ] {
            assert!(verdict(t), "should ask: {t}");
        }
        for t in [
            "Mi perro tiene cáncer y el veterinario quiere operarlo",
            "The vet says my dog has an ear infection",
            "Tenemos mucha deuda técnica en este módulo",
            "How does chemotherapy work?",
            "In my novel the main character dies of cancer",
            "Write a short story about a detective",
        ] {
            assert!(!verdict(t), "should not ask: {t}");
        }
    }

    #[test]
    fn passages_cover_the_message() {
        assert_eq!(passage_ranges(0), vec![(0, 0)]);
        assert_eq!(passage_ranges(50), vec![(0, 50)]);
        assert_eq!(passage_ranges(250), vec![(0, 120), (120, 240), (240, 250)]);
    }

    #[test]
    fn lexical_measure_ignores_accents_and_case() {
        let a = terms("¿Cuánta AGUA lleva la masa madre?");
        let b = terms("La masa madre lleva agua y harina");
        assert!(overlap(&a, &b) > 0.5, "{a:?} {b:?}");
        assert!(overlap(&a, &terms("Python sorted lambda")) < 0.01);
    }

    #[test]
    fn lexical_summary_requests() {
        for yes in [
            "Resume la conversación",
            "¿Me haces un resumen de lo que hemos hablado?",
            "Summarize our chat",
            "Can you recap what we discussed?",
            "Recapitula",
        ] {
            assert!(lexical_summary_request(yes), "{yes}");
        }
        for no in [
            "Resume este texto: el gato duerme",
            "Write a summary of Hamlet",
            "¿Qué es un resumen ejecutivo?",
        ] {
            assert!(!lexical_summary_request(no), "{no}");
        }
    }

    #[test]
    fn questions_that_refer_back() {
        for yes in [
            "¿Y en qué barrio me dijiste que había fado?",
            "Lo del compilador, ¿cómo era la solución?",
            "Montre-moi encore la différence entre les jointures",
            "¿Cómo activaba el entorno en Windows?",
            "What did you say earlier about the oven?",
            "Remind me the proxy line",
        ] {
            assert!(refers_back(yes), "{yes}");
        }
        for no in [
            "¿Cómo cambio una rueda pinchada?",
            "How do I knit a scarf?",
            "¿Qué significa resiliencia?",
            "Recomiéndame una serie",
        ] {
            assert!(!refers_back(no), "{no}");
        }
    }

    #[test]
    fn lexical_sensitive_topics() {
        assert!(lexical_sensitive("Me diagnosticaron depresión"));
        assert!(lexical_sensitive("my password is hunter2"));
        assert!(lexical_sensitive("Estoy embarazada de ocho semanas"));
        assert!(lexical_sensitive("I'm HIV positive"));
        assert!(!lexical_sensitive("¿Cómo se hace una tortilla?"));
    }

    #[test]
    fn everyday_words_are_not_sensitive() {
        for no in [
            "Sin embargo, el código no compila",
            "Tenemos mucha deuda técnica en este módulo",
            "Hay que diagnosticar el error antes de seguir",
            "Que se divida la tarea en dos",
            "El proceso está detenido",
            "La terapia génica es un campo nuevo",
            "How do I hash a password in Rust?",
        ] {
            assert!(!lexical_sensitive(no), "{no}");
        }
    }

    #[test]
    fn only_the_persons_own_messages_ask_first() {
        let turn = |text, from_me| Turn {
            text,
            from_me,
            private: false,
            vectors: None,
        };
        assert!(sensitive_turn(&turn(
            "Me diagnosticaron asma el año pasado",
            true
        )));
        assert!(!sensitive_turn(&turn(
            "Me diagnosticaron asma el año pasado",
            false
        )));
        assert!(!sensitive_turn(&turn("Mi amiga Ana viene mañana", true)));
    }

    #[test]
    fn without_the_model_a_related_message_is_still_found() {
        let texts = [
            "Receta de masa madre: harina y agua a partes iguales",
            "Mezcla 50 g de harina con 50 g de agua cada día",
            "¿Cómo ordeno una lista en Python?",
            "Usa sorted con key",
            "¿Y para Lisboa?",
            "Alfama y Chiado",
        ];
        let turns: Vec<Turn> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| Turn {
                text: t,
                from_me: i % 2 == 0,
                private: false,
                vectors: None,
            })
            .collect();
        let p = choose(
            &turns,
            "¿Cuánta agua le ponía a la masa madre?",
            None,
            &Rules::default(),
        )
        .unwrap();
        assert_eq!(p.recent, vec![4, 5]);
        let picked: Vec<usize> = p.older.iter().map(|f| f.index).collect();
        assert!(picked.contains(&0) || picked.contains(&1), "{picked:?}");
        assert!(!picked.contains(&2) && !picked.contains(&3), "{picked:?}");
    }
}
