//! WebSocket chat handler and in-process connection registry.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::ws::{Message, Utf8Bytes};
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

/// Per-WS connection sender (one per upgraded WebSocket).
pub type ConnTx = UnboundedSender<Message>;

#[derive(Default)]
pub struct WsConnections {
    inner: Mutex<HashMap<String, Vec<(Uuid, ConnTx)>>>,
}

impl WsConnections {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, username: &str, id: Uuid, sender: ConnTx) {
        let mut map = self.inner.lock().expect("ws conn map poisoned");
        map.entry(username.to_string()).or_default().push((id, sender));
    }

    pub fn unregister(&self, username: &str, id: Uuid) {
        let mut map = self.inner.lock().expect("ws conn map poisoned");
        if let Some(list) = map.get_mut(username) {
            list.retain(|(other, _)| *other != id);
            if list.is_empty() {
                map.remove(username);
            }
        }
    }

    pub fn send_to(&self, username: &str, payload: &str) {
        let map = self.inner.lock().expect("ws conn map poisoned");
        if let Some(list) = map.get(username) {
            for (_, tx) in list {
                let _ = tx.send(Message::Text(Utf8Bytes::from(payload)));
            }
        }
    }

    pub fn send_to_except(&self, username: &str, except: Uuid, payload: &str) {
        let map = self.inner.lock().expect("ws conn map poisoned");
        if let Some(list) = map.get(username) {
            for (id, tx) in list {
                if *id != except {
                    let _ = tx.send(Message::Text(Utf8Bytes::from(payload)));
                }
            }
        }
    }
}
