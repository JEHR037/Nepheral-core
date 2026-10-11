// SPDX-License-Identifier: BUSL-1.1
//! Conversation memory decided on the device (docs/CONVERSATIONS.md).
//!
//! `select` holds the rules: which earlier messages a question carries, and
//! when the person must confirm. `embed` turns text into vectors with a
//! small multilingual model, so "related" means related in meaning, in any
//! language. `memory` joins both for a conversation, with a lexical measure
//! for when the model is not on the device yet.

pub mod embed;
pub mod exemplars;
pub mod memory;
pub mod select;
mod xlmr;
