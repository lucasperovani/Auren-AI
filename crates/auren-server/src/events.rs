//! Live event channel (spec: docs/PROMPT.md section 4, docs/api/websocket.md).
//!
//! The WebSocket is a live-update channel only; it is never the source of
//! truth for task state. Important events are persisted by their owning
//! service; this module only fans out in-memory copies to connected clients.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

/// Capacity of the per-process broadcast channel. Slow clients drop events
/// rather than blocking producers; clients recover via REST on reconnect.
const BROADCAST_CAPACITY: usize = 1024;

/// Canonical event envelope transmitted on `/api/v1/events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: String,
    pub event_type: String,
    pub timestamp: String,
    pub data: serde_json::Value,
}

impl EventEnvelope {
    pub fn new(event_type: &str, data: serde_json::Value) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            data,
        }
    }
}

/// Fan-out hub for live events. Services publish here; WebSocket handlers
/// subscribe per connection.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<EventEnvelope>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self { tx }
    }

    /// Publish an event to all connected clients. Returns the number of
    /// receivers that got the event; zero is normal when nobody is connected.
    pub fn publish(&self, event: EventEnvelope) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe a new receiver. The receiver only sees events published
    /// after this call.
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.tx.subscribe()
    }
}

/// A single-use WebSocket ticket. The client obtains it over an
/// authenticated REST call and presents it in the upgrade request, so no
/// reusable session token ever appears in a URL or query string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsTicket {
    pub ticket_id: String,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
}

/// In-memory store of unspent tickets. Tickets are consumed atomically on
/// first use and expire quickly.
#[derive(Default)]
pub struct TicketStore {
    tickets: std::sync::Mutex<HashMap<String, WsTicket>>,
}

impl TicketStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a short-lived ticket bound to the authenticated user.
    pub fn issue(&self, user_id: &str, ttl_secs: i64) -> WsTicket {
        let ticket = WsTicket {
            ticket_id: Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            expires_at: Utc::now() + chrono::Duration::seconds(ttl_secs),
        };
        self.tickets
            .lock()
            .expect("ticket store poisoned")
            .insert(ticket.ticket_id.clone(), ticket.clone());
        ticket
    }

    /// Atomically consume a ticket. Returns the ticket only when it exists,
    /// is unspent, and has not expired; otherwise `None`.
    pub fn consume(&self, ticket_id: &str) -> Option<WsTicket> {
        let mut tickets = self.tickets.lock().expect("ticket store poisoned");
        let ticket = tickets.remove(ticket_id)?;
        if ticket.expires_at < Utc::now() {
            return None;
        }
        Some(ticket)
    }

    /// Drop expired tickets. Called opportunistically on issuance.
    pub fn prune(&self) {
        let now = Utc::now();
        self.tickets
            .lock()
            .expect("ticket store poisoned")
            .retain(|_, t| t.expires_at >= now);
    }
}

/// Shared event infrastructure handed to handlers and services.
pub type SharedEventBus = Arc<EventBus>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_has_required_fields() {
        let e = EventEnvelope::new("task.started", serde_json::json!({ "task_id": "t1" }));
        assert_eq!(e.event_type, "task.started");
        assert!(!e.event_id.is_empty());
        assert!(!e.timestamp.is_empty());
        assert_eq!(e.data["task_id"], "t1");
    }

    #[test]
    fn bus_delivers_to_subscribers_only() {
        let bus = EventBus::new();
        // Events published before any subscriber are dropped.
        bus.publish(EventEnvelope::new("task.created", serde_json::json!({})));

        let mut rx = bus.subscribe();
        bus.publish(EventEnvelope::new("task.started", serde_json::json!({ "task_id": "t1" })));
        let received = rx.try_recv().unwrap();
        assert_eq!(received.event_type, "task.started");
    }

    #[test]
    fn ticket_is_single_use_and_expiring() {
        let store = TicketStore::new();
        let ticket = store.issue("user-1", 30);

        // First consume succeeds, second fails (single use).
        let consumed = store.consume(&ticket.ticket_id).unwrap();
        assert_eq!(consumed.user_id, "user-1");
        assert!(store.consume(&ticket.ticket_id).is_none());

        // Unknown ticket fails.
        assert!(store.consume("nope").is_none());
    }

    #[test]
    fn expired_ticket_is_rejected() {
        let store = TicketStore::new();
        let ticket = store.issue("user-1", -1); // already expired
        assert!(store.consume(&ticket.ticket_id).is_none());
    }
}