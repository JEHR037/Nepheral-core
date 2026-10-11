// SPDX-License-Identifier: BUSL-1.1

use core_crypto::extrinsic::PALLET_REPUTATION;
use core_crypto::pallets::reputation::{ReputationCall, Verdict, MAX_NOTE_CHARS, MIN_NOTE_CHARS};

use crate::agent::RatingRef;
use crate::state::AppState;

fn check_verdict(verdict: &Verdict) -> Result<(), String> {
    if let Verdict::Dislike { note, .. } = verdict {
        let chars = note.trim().chars().count();
        if !(MIN_NOTE_CHARS..=MAX_NOTE_CHARS).contains(&chars) {
            return Err(format!(
                "a dislike needs a reason of {MIN_NOTE_CHARS} to {MAX_NOTE_CHARS} characters"
            ));
        }
    }
    Ok(())
}

impl AppState {
    async fn submit_rating(&self, rating: RatingRef, verdict: Verdict) {
        let verdict = match verdict {
            Verdict::Dislike { reason, note } => Verdict::Dislike {
                reason,
                note: note.trim().to_string(),
            },
            v => v,
        };
        self.sign_and_submit(
            PALLET_REPUTATION,
            &ReputationCall::Rate {
                query_digest: rating.query_digest,
                verdict,
            },
        )
        .await;
    }

    pub async fn rate_session_reply(
        &self,
        session_id: u64,
        index: usize,
        verdict: Verdict,
    ) -> Result<(), String> {
        check_verdict(&verdict)?;
        let like = matches!(verdict, Verdict::Like);
        let rating = {
            let mut sessions = self.agent_sessions.write().await;
            let entry = sessions
                .get_mut(&session_id)
                .and_then(|s| s.messages.get_mut(index))
                .ok_or_else(|| "unknown message".to_string())?;
            let rating = entry
                .rating
                .ok_or_else(|| "only answers from the mesh can be rated".to_string())?;
            if entry.rated.is_some() {
                return Err("this answer was already rated".into());
            }
            entry.rated = Some(like);
            rating
        };
        self.submit_rating(rating, verdict).await;
        self.agent_session_notify.notify_waiters();
        Ok(())
    }

    pub async fn rate_room_reply(
        &self,
        code: &str,
        message_id: u64,
        verdict: Verdict,
    ) -> Result<(), String> {
        check_verdict(&verdict)?;
        let like = matches!(verdict, Verdict::Like);
        let rating = {
            let mut rooms = self.rooms.write().await;
            let entry = rooms
                .get_mut(code)
                .and_then(|r| r.chat.iter_mut().find(|e| e.id == message_id))
                .ok_or_else(|| "unknown message".to_string())?;
            let rating = entry
                .rating
                .ok_or_else(|| "only the member who asked can rate this answer".to_string())?;
            if entry.rated.is_some() {
                return Err("this answer was already rated".into());
            }
            entry.rated = Some(like);
            rating
        };
        self.submit_rating(rating, verdict).await;
        self.room_notify.notify_waiters();
        Ok(())
    }
}
