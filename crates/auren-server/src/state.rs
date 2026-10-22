use std::sync::Arc;

use crate::{config::AppConfig, db::DbPool, events::EventBus, events::TicketStore};

/// Shared application state handed to all handlers and services.
#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub config: AppConfig,
    /// Live event fan-out for the WebSocket channel.
    pub events: Arc<EventBus>,
    /// Single-use WebSocket upgrade tickets.
    pub ws_tickets: Arc<TicketStore>,
}