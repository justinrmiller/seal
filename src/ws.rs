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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::{self, UnboundedReceiver};

    fn conn() -> (Uuid, ConnTx, UnboundedReceiver<Message>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Uuid::new_v4(), tx, rx)
    }

    /// Drain a receiver into the list of text payloads it currently holds.
    fn drain(rx: &mut UnboundedReceiver<Message>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let Message::Text(t) = msg {
                out.push(t.to_string());
            }
        }
        out
    }

    #[test]
    fn send_to_reaches_every_connection_of_a_user() {
        let conns = WsConnections::new();
        let (id1, tx1, mut rx1) = conn();
        let (id2, tx2, mut rx2) = conn();
        conns.register("alice", id1, tx1);
        conns.register("alice", id2, tx2);

        conns.send_to("alice", "hello");
        assert_eq!(drain(&mut rx1), vec!["hello"]);
        assert_eq!(drain(&mut rx2), vec!["hello"]);
    }

    #[test]
    fn send_to_except_skips_the_excluded_connection() {
        let conns = WsConnections::new();
        let (id1, tx1, mut rx1) = conn();
        let (id2, tx2, mut rx2) = conn();
        conns.register("alice", id1, tx1);
        conns.register("alice", id2, tx2);

        // Echo to alice's *other* devices, excluding the originating one (id1).
        conns.send_to_except("alice", id1, "echo");
        assert!(drain(&mut rx1).is_empty(), "originating conn must be skipped");
        assert_eq!(drain(&mut rx2), vec!["echo"]);
    }

    #[test]
    fn sending_to_unknown_user_is_a_noop() {
        let conns = WsConnections::new();
        // Neither call should panic when the user has no connections.
        conns.send_to("ghost", "x");
        conns.send_to_except("ghost", Uuid::new_v4(), "x");
    }

    #[test]
    fn unregister_removes_connection_and_drops_empty_user() {
        let conns = WsConnections::new();
        let (id1, tx1, mut rx1) = conn();
        let (id2, tx2, mut rx2) = conn();
        conns.register("alice", id1, tx1);
        conns.register("alice", id2, tx2);

        // Remove one of two: the user remains, only the survivor receives.
        conns.unregister("alice", id1);
        conns.send_to("alice", "after-one");
        assert!(drain(&mut rx1).is_empty());
        assert_eq!(drain(&mut rx2), vec!["after-one"]);

        // Remove the last connection: the user key is dropped entirely, so a
        // later send reaches nobody (exercises the empty-list removal branch).
        conns.unregister("alice", id2);
        conns.send_to("alice", "after-all");
        assert!(drain(&mut rx2).is_empty());
    }
}
