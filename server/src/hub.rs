//! Live change notifications.
//!
//! When a workspace's revision advances, every connected member is told the
//! new head. The notification carries **no data** — only a number. Clients
//! treat it purely as a hint to pull, so a missed, duplicated or
//! out-of-order notification cannot corrupt anything. Correctness lives
//! entirely in the pull/push protocol; this is an optimisation over
//! polling.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

/// How many notifications a slow client may fall behind before it is
/// dropped from the channel.
///
/// Falling behind is survivable: the client reconnects and pulls, which it
/// would have done anyway. Buffering without limit to spare it would let
/// one stalled websocket consume memory indefinitely.
const CHANNEL_CAPACITY: usize = 32;

/// What a subscriber receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Notification {
    /// The workspace has advanced to this revision.
    #[serde(rename_all = "camelCase")]
    Head {
        /// The new revision.
        seq: i64,
    },
}

/// Fans change notifications out to connected clients.
#[derive(Clone, Default)]
pub struct Hub {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<Notification>>>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.channels.lock().map(|c| c.len()).unwrap_or(0);
        f.debug_struct("Hub").field("workspaces", &count).finish()
    }
}

impl Hub {
    /// An empty hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes to a workspace's notifications.
    pub fn subscribe(&self, workspace_id: &str) -> broadcast::Receiver<Notification> {
        let mut channels = match self.channels.lock() {
            Ok(guard) => guard,
            // A poisoned lock here would mean a panic while fanning out.
            // Notifications are a hint, so recovering beats propagating.
            Err(poisoned) => poisoned.into_inner(),
        };

        channels
            .entry(workspace_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe()
    }

    /// Announces a workspace's new head.
    ///
    /// Does nothing when nobody is listening, which is the common case for
    /// a single-user server.
    pub fn publish(&self, workspace_id: &str, seq: i64) {
        let Ok(channels) = self.channels.lock() else {
            return;
        };
        if let Some(sender) = channels.get(workspace_id) {
            // An error means no receivers; that is not a problem.
            let _ = sender.send(Notification::Head { seq });
        }
    }

    /// Drops channels nobody is listening to.
    ///
    /// Without this, a long-lived server accumulates one empty channel per
    /// workspace anyone ever opened.
    pub fn prune(&self) {
        let Ok(mut channels) = self.channels.lock() else {
            return;
        };
        channels.retain(|_, sender| sender.receiver_count() > 0);
    }

    /// How many workspaces currently have channels.
    pub fn tracked_workspaces(&self) -> usize {
        self.channels.lock().map(|c| c.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_subscriber_receives_a_published_head() {
        let hub = Hub::new();
        let mut receiver = hub.subscribe("w1");

        hub.publish("w1", 7);

        assert_eq!(
            receiver.recv().await.unwrap(),
            Notification::Head { seq: 7 }
        );
    }

    #[tokio::test]
    async fn every_subscriber_to_a_workspace_is_told() {
        let hub = Hub::new();
        let mut a = hub.subscribe("w1");
        let mut b = hub.subscribe("w1");

        hub.publish("w1", 3);

        assert_eq!(a.recv().await.unwrap(), Notification::Head { seq: 3 });
        assert_eq!(b.recv().await.unwrap(), Notification::Head { seq: 3 });
    }

    #[tokio::test]
    async fn notifications_do_not_cross_workspaces() {
        let hub = Hub::new();
        let mut other = hub.subscribe("w2");

        hub.publish("w1", 5);

        // Nothing for w2; a member of one workspace learns nothing about
        // another, not even that it changed.
        assert!(other.try_recv().is_err());
    }

    #[test]
    fn publishing_with_no_listeners_is_harmless() {
        let hub = Hub::new();
        hub.publish("nobody-listening", 1);
        assert_eq!(hub.tracked_workspaces(), 0);
    }

    #[test]
    fn pruning_drops_channels_nobody_is_listening_to() {
        let hub = Hub::new();
        let receiver = hub.subscribe("w1");
        assert_eq!(hub.tracked_workspaces(), 1);

        hub.prune();
        assert_eq!(hub.tracked_workspaces(), 1, "a live listener was dropped");

        drop(receiver);
        hub.prune();
        assert_eq!(hub.tracked_workspaces(), 0);
    }

    #[tokio::test]
    async fn a_notification_carries_no_workspace_content() {
        // It is a hint to pull, not a data channel: anything it carried
        // would bypass the authorisation the pull path performs.
        let json = serde_json::to_value(Notification::Head { seq: 42 }).unwrap();
        assert_eq!(json["type"], "head");
        assert_eq!(json["seq"], 42);
        assert_eq!(json.as_object().unwrap().len(), 2);
    }
}
