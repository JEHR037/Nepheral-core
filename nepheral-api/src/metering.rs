// SPDX-License-Identifier: BUSL-1.1
//! When a premium request is paid for.
//!
//! A Top-Tier request spends one inference from an on-chain session. The spend
//! used to happen at dispatch, before anyone had answered — so every timeout,
//! every channel with no responder, every dropped connection still cost the
//! caller a credit. Here the spend is tied to the first real token of the
//! answer: no answer, no charge.

use futures::future::BoxFuture;
use futures::StreamExt;

use crate::backend::{BackendEvent, EventStream};

/// Submits the on-chain spend. Boxed so the caller decides what "charge"
/// means and this module never has to know about wallets or keys.
pub type ChargeFn = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

/// Passes `inner` through unchanged, running `charge` exactly once — the
/// moment the first non-empty token arrives. Status lines, errors, an empty
/// answer or a stream that ends without answering never trigger it.
///
/// The charge runs before that token is yielded, so a consumer that reads one
/// token and hangs up has still paid for an answer it received.
pub fn charge_on_first_answer(inner: EventStream, charge: ChargeFn) -> EventStream {
    let stream = async_stream::stream! {
        let mut inner = inner;
        let mut charge = Some(charge);
        while let Some(ev) = inner.next().await {
            if let BackendEvent::Token(t) = &ev {
                if !t.is_empty() {
                    if let Some(c) = charge.take() {
                        c().await;
                    }
                }
            }
            yield ev;
        }
    };
    Box::pin(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn events(evs: Vec<BackendEvent>) -> EventStream {
        Box::pin(futures::stream::iter(evs))
    }

    fn counter() -> (Arc<AtomicUsize>, ChargeFn) {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let charge: ChargeFn = Box::new(move || {
            Box::pin(async move {
                n2.fetch_add(1, Ordering::SeqCst);
            })
        });
        (n, charge)
    }

    async fn drain(s: EventStream) -> Vec<BackendEvent> {
        s.collect().await
    }

    fn is_token(ev: &BackendEvent, text: &str) -> bool {
        matches!(ev, BackendEvent::Token(t) if t == text)
    }

    #[tokio::test]
    async fn an_answer_is_charged_exactly_once() {
        let (n, charge) = counter();
        let out = drain(charge_on_first_answer(
            events(vec![
                BackendEvent::Status("waiting".into()),
                BackendEvent::Token("hello".into()),
                BackendEvent::Token(" world".into()),
                BackendEvent::Done,
            ]),
            charge,
        ))
        .await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert_eq!(out.len(), 4, "the stream passes through unchanged");
        assert!(is_token(&out[1], "hello"));
    }

    #[tokio::test]
    async fn a_timeout_is_never_charged() {
        // This is the bug being fixed: a channel nobody answers used to cost
        // a credit every time.
        let (n, charge) = counter();
        drain(charge_on_first_answer(
            events(vec![
                BackendEvent::Status("submitted to mesh".into()),
                BackendEvent::Status("still waiting".into()),
                BackendEvent::Error("mesh response timeout".into()),
            ]),
            charge,
        ))
        .await;
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_empty_answer_is_not_an_answer() {
        let (n, charge) = counter();
        drain(charge_on_first_answer(
            events(vec![BackendEvent::Token(String::new()), BackendEvent::Done]),
            charge,
        ))
        .await;
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_charge_waits_for_the_first_real_token() {
        let (n, charge) = counter();
        let out = drain(charge_on_first_answer(
            events(vec![
                BackendEvent::Token(String::new()),
                BackendEvent::Token("x".into()),
                BackendEvent::Done,
            ]),
            charge,
        ))
        .await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert!(is_token(&out[1], "x"));
    }

    #[tokio::test]
    async fn an_empty_stream_charges_nothing() {
        let (n, charge) = counter();
        drain(charge_on_first_answer(events(vec![]), charge)).await;
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn hanging_up_after_the_first_token_still_pays() {
        // The consumer got an answer; leaving early does not undo that.
        let (n, charge) = counter();
        let mut s = charge_on_first_answer(
            events(vec![
                BackendEvent::Token("a".into()),
                BackendEvent::Token("b".into()),
            ]),
            charge,
        );
        let first = s.next().await;
        drop(s);
        assert!(matches!(first, Some(BackendEvent::Token(_))));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
