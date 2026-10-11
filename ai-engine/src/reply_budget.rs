// SPDX-License-Identifier: BUSL-1.1
//! How a reply is fitted into a model's context, and how reasoning is kept
//! from eating the whole reply.
//!
//! A reasoning model writes inside `<think>…</think>` before it answers, and
//! the reasoning and the answer share one token budget. Whichever comes first
//! can take all of it: a model that reasons for the whole budget returns
//! nothing to show. Three defences, cheapest first:
//!
//! 1. Reasoning is off unless asked for. For a template that knows a reasoning
//!    block, the prompt is pre-filled with an empty one - what the template's
//!    own `enable_thinking = false` renders - so the model answers directly.
//! 2. When reasoning is allowed, the reply budget is raised to leave it room.
//! 3. Whatever the model does, a reasoning block that reaches its share of the
//!    budget is closed for it, leaving the rest for the answer.
//!
//! None of this depends on llama.cpp, so it is tested without a model.

use crate::types::{Message, MessageRole};

/// Closes a reasoning block the model did not close itself.
pub const CLOSE_REASONING: &str = "\n</think>\n\n";

/// The smallest reply budget a reasoning model is given (defence 2).
pub const REASONING_MIN_TOKENS: usize = 2048;

/// Pre-fills an empty reasoning block, so a reasoning model answers directly.
///
/// Matches what Qwen3's template renders for `enable_thinking = false`. A
/// template that already opens the block at the end of the prompt (forced
/// reasoning, as in R1 distills) only needs it closed.
pub fn without_reasoning(prompt: &str) -> String {
    if prompt.trim_end().ends_with("<think>") {
        format!("{}{}", prompt.trim_end(), CLOSE_REASONING)
    } else {
        format!("{prompt}<think>\n\n</think>\n\n")
    }
}

/// Whether a prompt ends inside an open reasoning block.
pub fn ends_inside_reasoning(prompt: &str) -> bool {
    let tail = prompt.trim_end();
    match (tail.rfind("<think>"), tail.rfind("</think>")) {
        (Some(open), Some(close)) => open > close,
        (Some(_), None) => true,
        _ => false,
    }
}

/// How many of `max_tokens` reasoning may use before it is closed, leaving
/// the rest for the answer: a quarter, at least 128 and at most half.
pub fn reasoning_share(max_tokens: usize) -> usize {
    let reserve = (max_tokens / 4).max(128).min(max_tokens / 2);
    max_tokens.saturating_sub(reserve)
}

/// Watches a reply as it is generated and says when a reasoning block has
/// used its share of the budget (defence 3).
#[derive(Debug)]
pub struct ReasoningGuard {
    inside: bool,
    used: usize,
    share: usize,
    tail: String,
}

impl ReasoningGuard {
    pub fn new(starts_inside: bool, share: usize) -> Self {
        Self {
            inside: starts_inside,
            used: 0,
            share,
            tail: String::new(),
        }
    }

    pub fn inside(&self) -> bool {
        self.inside
    }

    /// Feeds one generated piece (one token). Returns true when the block
    /// must be closed now.
    pub fn on_piece(&mut self, piece: &str) -> bool {
        self.tail.push_str(piece);
        // Tags can arrive split across tokens; keep just enough to see one.
        if self.tail.len() > 32 {
            let cut = self.tail.len() - 16;
            let cut = (cut..self.tail.len())
                .find(|&i| self.tail.is_char_boundary(i))
                .unwrap_or(self.tail.len());
            self.tail.drain(..cut);
        }
        if !self.inside && self.tail.contains("<think>") {
            self.inside = true;
            self.used = 0;
            self.tail.clear();
            return false;
        }
        if self.inside {
            if self.tail.contains("</think>") {
                self.inside = false;
                self.tail.clear();
                return false;
            }
            self.used += 1;
            return self.used >= self.share;
        }
        false
    }

    /// Records that the block was closed for the model.
    pub fn closed(&mut self) {
        self.inside = false;
        self.tail.clear();
    }
}

/// A prompt rendered to fit the context, and how many older turns it lost.
#[derive(Debug, PartialEq, Eq)]
pub struct Fitted {
    pub prompt: String,
    pub dropped: usize,
}

/// Renders `messages`, dropping the oldest turns until the prompt fits in
/// `max_prompt` tokens.
///
/// The system prompt and the latest message always stay: they are what the
/// reply is for. Cutting the oldest *tokens* instead, as this used to, removed
/// the system prompt and the template's own turn markers first and left the
/// model a prompt it could not parse. If the system prompt and the latest
/// message alone are too long, the prompt is returned as it is and the caller
/// decides.
pub fn fit_messages<R, C>(messages: &[Message], max_prompt: usize, render: R, count: C) -> Fitted
where
    R: Fn(&[Message]) -> String,
    C: Fn(&str) -> usize,
{
    let mut kept: Vec<Message> = messages.to_vec();
    let mut dropped = 0;
    loop {
        let prompt = render(&kept);
        if count(&prompt) <= max_prompt {
            return Fitted { prompt, dropped };
        }
        let last = kept.len().saturating_sub(1);
        let oldest = kept
            .iter()
            .position(|m| !matches!(m.role, MessageRole::System))
            .filter(|&i| i < last);
        match oldest {
            Some(i) => {
                kept.remove(i);
                dropped += 1;
            }
            None => return Fitted { prompt, dropped },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            timestamp: 0,
        }
    }

    fn render(ms: &[Message]) -> String {
        ms.iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("|")
    }

    fn words(s: &str) -> usize {
        s.split(['|', ' ']).filter(|w| !w.is_empty()).count()
    }

    #[test]
    fn reasoning_is_prefilled_empty_or_closed_when_forced() {
        assert_eq!(
            without_reasoning("<|im_start|>assistant\n"),
            "<|im_start|>assistant\n<think>\n\n</think>\n\n"
        );
        assert_eq!(
            without_reasoning("<|Assistant|><think>\n"),
            "<|Assistant|><think>\n</think>\n\n"
        );
    }

    #[test]
    fn an_open_block_at_the_end_of_the_prompt_is_noticed() {
        assert!(ends_inside_reasoning("x<think>\n"));
        assert!(!ends_inside_reasoning("x<think></think>\n"));
        assert!(!ends_inside_reasoning("plain prompt"));
    }

    #[test]
    fn the_answer_always_keeps_a_reserve() {
        assert_eq!(reasoning_share(512), 384);
        assert_eq!(reasoning_share(2048), 1536);
        assert_eq!(reasoning_share(200), 100, "never more than half reserved");
    }

    #[test]
    fn a_long_reasoning_block_is_closed_at_its_share() {
        let mut g = ReasoningGuard::new(false, 3);
        assert!(!g.on_piece("<thi"));
        assert!(!g.on_piece("nk>"));
        assert!(g.inside());
        assert!(!g.on_piece("a"));
        assert!(!g.on_piece("b"));
        assert!(
            g.on_piece("c"),
            "third token inside the block hits the share"
        );
        g.closed();
        assert!(!g.inside());
        assert!(!g.on_piece("answer"));
    }

    #[test]
    fn a_block_the_model_closes_itself_is_left_alone() {
        let mut g = ReasoningGuard::new(true, 10);
        assert!(!g.on_piece("short"));
        assert!(!g.on_piece("</thi"));
        assert!(!g.on_piece("nk>"));
        assert!(!g.inside());
        for _ in 0..50 {
            assert!(!g.on_piece("answer"));
        }
    }

    #[test]
    fn oldest_turns_go_first_and_the_system_prompt_and_question_stay() {
        let ms = [
            msg(MessageRole::System, "be brief"),
            msg(MessageRole::User, "one two three"),
            msg(MessageRole::Assistant, "four five six"),
            msg(MessageRole::User, "latest question"),
        ];
        let f = fit_messages(&ms, 6, render, words);
        assert_eq!(f.prompt, "be brief|latest question");
        assert_eq!(f.dropped, 2);

        let f = fit_messages(&ms, 100, render, words);
        assert_eq!(f.dropped, 0);
    }

    #[test]
    fn what_cannot_shrink_further_is_returned_for_the_caller() {
        let ms = [
            msg(MessageRole::System, "a b c d e"),
            msg(MessageRole::User, "f g h i j"),
        ];
        let f = fit_messages(&ms, 3, render, words);
        assert_eq!(f.dropped, 0);
        assert_eq!(f.prompt, "a b c d e|f g h i j");
    }
}
