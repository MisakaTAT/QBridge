use super::{Session, SessionStore};
use crate::redact;
use std::sync::Arc;
use tokio::sync::Mutex;

#[must_use = "acknowledge only after the dispatch has been handled"]
pub struct GatewayAcknowledgement {
    writer: Arc<CheckpointWriter>,
    session: Arc<str>,
    sequence: u64,
    intents: u64,
}

impl GatewayAcknowledgement {
    pub async fn acknowledge(self) {
        let mut active = self.writer.active.lock().await;
        let Some(active) = active.as_mut() else {
            return;
        };
        if !Arc::ptr_eq(&self.session, &active.id)
            || active.saved.is_some_and(|seq| self.sequence <= seq)
        {
            return;
        }
        let checkpoint = Session {
            session_id: self.session.to_string(),
            seq: self.sequence,
            intents: self.intents,
        };
        match self.writer.store.save(&checkpoint).await {
            Ok(()) => active.saved = Some(self.sequence),
            Err(error) => tracing::warn!(
                reason = %redact::text(&error.to_string()),
                "cannot persist Gateway checkpoint"
            ),
        }
    }
}

struct ActiveSession {
    id: Arc<str>,
    saved: Option<u64>,
}

pub(super) struct CheckpointWriter {
    pub(super) store: Arc<dyn SessionStore>,
    active: Mutex<Option<ActiveSession>>,
}

impl CheckpointWriter {
    pub(super) fn new(store: Arc<dyn SessionStore>) -> Self {
        Self {
            store,
            active: Mutex::new(None),
        }
    }

    pub(super) async fn activate(&self, id: &str, saved: Option<u64>) {
        *self.active.lock().await = Some(ActiveSession {
            id: Arc::from(id),
            saved,
        });
    }

    pub(super) async fn clear(&self) {
        let mut active = self.active.lock().await;
        *active = None;
        if let Err(error) = self.store.clear().await {
            tracing::warn!(
                reason = %redact::text(&error.to_string()),
                "cannot clear Gateway checkpoint"
            );
        }
    }

    pub(super) async fn acknowledgement(
        self: &Arc<Self>,
        sequence: Option<u64>,
        intents: u64,
    ) -> Option<GatewayAcknowledgement> {
        let sequence = sequence?;
        let session = self.active.lock().await.as_ref()?.id.clone();
        Some(GatewayAcknowledgement {
            writer: self.clone(),
            session,
            sequence,
            intents,
        })
    }
}
