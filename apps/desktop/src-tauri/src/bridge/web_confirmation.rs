use super::{DesktopFailure, main_window};
use crate::confirmation::CONFIRMATION_TTL;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{Emitter, EventTarget, Manager, State, WebviewWindow};
use tokio::sync::oneshot;

pub(super) const WEB_CONFIRMATION_EVENT: &str = "hiroute-web-confirmation";
const WEB_CONFIRMATION_SCHEMA: &str = "hiroute.web-confirmation/v1";

pub(super) struct WebConfirmationPrompt {
    pub title: String,
    pub message: String,
    pub confirm_label: String,
    pub cancel_label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct WebConfirmationRequest {
    schema: &'static str,
    confirmation_id: String,
    title: String,
    message: String,
    confirm_label: String,
    cancel_label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WebConfirmationDecision {
    confirmation_id: String,
    accepted: bool,
}

struct PendingConfirmation {
    window_label: String,
    request: WebConfirmationRequest,
    sequence: u64,
    response: oneshot::Sender<bool>,
}

#[derive(Default)]
pub(super) struct WebConfirmationState {
    pending: Mutex<BTreeMap<String, PendingConfirmation>>,
    next_sequence: AtomicU64,
}

impl WebConfirmationState {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, PendingConfirmation>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn begin(
        &self,
        window_label: &str,
        prompt: WebConfirmationPrompt,
    ) -> Result<(WebConfirmationRequest, oneshot::Receiver<bool>), DesktopFailure> {
        let (response, receive) = oneshot::channel();
        let mut pending = self.lock();
        let sequence = self
            .next_sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| DesktopFailure::from("CONFIRMATION_UNAVAILABLE"))?;
        let confirmation_id = loop {
            let candidate = format!("confirmation/{}", crate::random_id()?);
            if !pending.contains_key(&candidate) {
                break candidate;
            }
        };
        let request = WebConfirmationRequest {
            schema: WEB_CONFIRMATION_SCHEMA,
            confirmation_id: confirmation_id.clone(),
            title: prompt.title,
            message: prompt.message,
            confirm_label: prompt.confirm_label,
            cancel_label: prompt.cancel_label,
        };
        pending.insert(
            confirmation_id.clone(),
            PendingConfirmation {
                window_label: window_label.to_owned(),
                request: request.clone(),
                sequence,
                response,
            },
        );
        Ok((request, receive))
    }

    fn snapshot(&self, window_label: &str) -> Option<WebConfirmationRequest> {
        self.lock()
            .values()
            .filter(|pending| pending.window_label == window_label)
            .min_by_key(|pending| pending.sequence)
            .map(|pending| pending.request.clone())
    }

    fn resolve(
        &self,
        window_label: &str,
        confirmation_id: &str,
        accepted: bool,
    ) -> Result<(), DesktopFailure> {
        if !valid_confirmation_id(confirmation_id) {
            return Err("CONFIRMATION_STALE".into());
        }
        let pending = {
            let mut confirmations = self.lock();
            if confirmations
                .get(confirmation_id)
                .is_none_or(|pending| pending.window_label != window_label)
            {
                return Err("CONFIRMATION_STALE".into());
            }
            confirmations
                .remove(confirmation_id)
                .expect("matching confirmation exists")
        };
        pending
            .response
            .send(accepted)
            .map_err(|_| DesktopFailure::from("CONFIRMATION_STALE"))
    }

    fn cancel(&self, window_label: &str, confirmation_id: &str) {
        let pending = {
            let mut confirmations = self.lock();
            if confirmations
                .get(confirmation_id)
                .is_some_and(|pending| pending.window_label == window_label)
            {
                confirmations.remove(confirmation_id)
            } else {
                None
            }
        };
        if let Some(pending) = pending {
            let _ = pending.response.send(false);
        }
    }

    pub(super) fn cancel_window(&self, window_label: &str) {
        let pending = {
            let mut confirmations = self.lock();
            let ids = confirmations
                .iter()
                .filter(|(_, pending)| pending.window_label == window_label)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| confirmations.remove(&id))
                .collect::<Vec<_>>()
        };
        for pending in pending {
            let _ = pending.response.send(false);
        }
    }
}

fn valid_confirmation_id(value: &str) -> bool {
    value.strip_prefix("confirmation/").is_some_and(|id| {
        id.len() == 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

pub(super) async fn request_web_confirmation(
    window: &WebviewWindow,
    prompt: WebConfirmationPrompt,
) -> bool {
    let state = window.state::<WebConfirmationState>();
    let Ok((request, receive)) = state.begin(window.label(), prompt) else {
        return false;
    };
    let confirmation_id = request.confirmation_id.clone();
    if window
        .emit_to(
            EventTarget::webview(window.label()),
            WEB_CONFIRMATION_EVENT,
            request,
        )
        .is_err()
    {
        state.cancel(window.label(), &confirmation_id);
    }
    let decision = tokio::time::timeout(CONFIRMATION_TTL, receive).await;
    if decision.is_err() {
        state.cancel(window.label(), &confirmation_id);
    }
    decision.ok().and_then(Result::ok).unwrap_or(false)
}

#[tauri::command]
pub(super) async fn web_confirmation_snapshot(
    window: WebviewWindow,
    state: State<'_, WebConfirmationState>,
) -> Result<Option<WebConfirmationRequest>, DesktopFailure> {
    main_window(&window)?;
    Ok(state.snapshot(window.label()))
}

#[tauri::command]
pub(super) async fn resolve_web_confirmation(
    window: WebviewWindow,
    state: State<'_, WebConfirmationState>,
    input: WebConfirmationDecision,
) -> Result<(), DesktopFailure> {
    main_window(&window)?;
    state.resolve(window.label(), &input.confirmation_id, input.accepted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(title: &str) -> WebConfirmationPrompt {
        WebConfirmationPrompt {
            title: title.to_owned(),
            message: "Apply the verified change.".to_owned(),
            confirm_label: "Apply change".to_owned(),
            cancel_label: "Cancel".to_owned(),
        }
    }

    #[tokio::test]
    async fn exact_window_and_id_resolve_each_confirmation_once() {
        let state = WebConfirmationState::default();
        let (request, receive) = state.begin("main", prompt("First")).unwrap();
        let id = request.confirmation_id;
        assert!(id.starts_with("confirmation/"));
        assert_eq!(id.len(), "confirmation/".len() + 64);
        assert!(state.resolve("other", &id, true).is_err());
        state.resolve("main", &id, true).unwrap();
        assert!(receive.await.unwrap());
        assert!(state.resolve("main", &id, true).is_err());
    }

    #[tokio::test]
    async fn closing_one_window_denies_only_its_pending_confirmations() {
        let state = WebConfirmationState::default();
        let (_, main_one) = state.begin("main", prompt("Main one")).unwrap();
        let (_, main_two) = state.begin("main", prompt("Main two")).unwrap();
        let (other_request, other) = state.begin("other", prompt("Other")).unwrap();
        state.cancel_window("main");
        assert!(!main_one.await.unwrap());
        assert!(!main_two.await.unwrap());
        state
            .resolve("other", &other_request.confirmation_id, true)
            .unwrap();
        assert!(other.await.unwrap());
    }

    #[test]
    fn snapshot_returns_only_the_oldest_confirmation_for_the_exact_window() {
        let state = WebConfirmationState::default();
        let (first, _) = state.begin("main", prompt("First")).unwrap();
        let (_, _) = state.begin("other", prompt("Other")).unwrap();
        let (_, _) = state.begin("main", prompt("Second")).unwrap();
        assert_eq!(state.snapshot("main"), Some(first));
        assert!(state.snapshot("missing").is_none());
    }

    #[test]
    fn decision_rejects_unknown_fields() {
        let value = serde_json::json!({
            "confirmation_id": format!("confirmation/{}", "0".repeat(64)),
            "accepted": true,
            "injected": "not accepted"
        });
        assert!(serde_json::from_value::<WebConfirmationDecision>(value).is_err());
    }

    #[test]
    fn confirmation_ids_are_exact_lowercase_random_tokens() {
        assert!(valid_confirmation_id(&format!(
            "confirmation/{}",
            "0a".repeat(32)
        )));
        assert!(!valid_confirmation_id("confirmation/1"));
        assert!(!valid_confirmation_id(&format!(
            "confirmation/{}",
            "A0".repeat(32)
        )));
        assert!(!valid_confirmation_id(&format!(
            "other/{}",
            "00".repeat(32)
        )));
    }
}
