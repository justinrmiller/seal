//! /ws/chat handler: accept upgrade, decode token, then loop on incoming
//! messages, dispatching to handle_dm / handle_channel and registering this
//! connection in the AppState's WsConnections so other handlers can relay.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth::decode_token;
use crate::db;
use crate::db_ops::{self, Cell};
use crate::models::{ChannelMessagePayload, TokenQuery};
use crate::routes::messages::{relay_channel_message, store_channel_message};
use crate::AppState;

pub async fn ws_chat(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(qp): Query<TokenQuery>,
) -> impl IntoResponse {
    let username = decode_token(&state.cfg, &qp.token);
    ws.on_upgrade(move |mut socket| async move {
        match username {
            Some(u) => handle_socket(socket, state, u).await,
            None => {
                let _ = socket
                    .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: 4001,
                        reason: Utf8Bytes::from_static("Invalid token"),
                    })))
                    .await;
            }
        }
    })
}

async fn handle_socket(socket: WebSocket, state: AppState, username: String) {
    let conn_id = Uuid::new_v4();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // Writer task: drains the mpsc into the WS sink.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    state
        .ws_connections
        .register(&username, conn_id, tx.clone());

    while let Some(Ok(msg)) = stream.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let data: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("ws: bad json from {username}: {e}");
                continue;
            }
        };
        match data.get("type").and_then(|v| v.as_str()).unwrap_or("dm") {
            "channel" => {
                if let Err(e) = handle_channel(&state, &username, conn_id, &tx, data).await {
                    tracing::warn!("ws channel handler error: {e:?}");
                }
            }
            _ => {
                if let Err(e) = handle_dm(&state, &username, conn_id, &tx, data).await {
                    tracing::warn!("ws dm handler error: {e:?}");
                }
            }
        }
    }

    state.ws_connections.unregister(&username, conn_id);
    drop(tx);
    let _ = writer.await;
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

async fn handle_dm(
    state: &AppState,
    sender: &str,
    own_id: Uuid,
    own_tx: &mpsc::UnboundedSender<Message>,
    data: Value,
) -> anyhow::Result<()> {
    let recipient = match data.get("recipient").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => {
            let _ = own_tx.send(text_msg(&json!({"error": "Invalid DM payload"})));
            return Ok(());
        }
    };
    let ciphertext = data.get("ciphertext").and_then(|v| v.as_str()).unwrap_or("");
    let iv = data.get("iv").and_then(|v| v.as_str()).unwrap_or("");
    let spk = data
        .get("sender_public_key_jwk")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let raw_type = data
        .get("message_type")
        .and_then(|v| v.as_str())
        .unwrap_or("text");
    let msg_type = if raw_type == "image" { "image" } else { "text" };

    let msg_id = Uuid::new_v4().to_string();
    let ts = now_secs();

    let messages = db_ops::open(&state.conn, "messages")
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let row = db_ops::mixed_row(
        db::messages_schema(),
        &[
            Cell::Str(&msg_id),
            Cell::Str(sender),
            Cell::Str(recipient),
            Cell::Str(""),
            Cell::Str(ciphertext),
            Cell::Str(iv),
            Cell::Str(spk),
            Cell::F64(ts),
            Cell::Str(msg_type),
            Cell::Str(""),
        ],
    )
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    db_ops::append(&messages, row)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    // Sender's self-copy (so they can decrypt their own DMs in history).
    let self_ct = data.get("self_ciphertext").and_then(|v| v.as_str());
    let self_iv = data.get("self_iv").and_then(|v| v.as_str());
    let self_spk = data
        .get("self_sender_public_key_jwk")
        .and_then(|v| v.as_str());
    if let (Some(self_ct), Some(self_iv), Some(self_spk)) = (self_ct, self_iv, self_spk) {
        let self_id = Uuid::new_v4().to_string();
        let row = db_ops::mixed_row(
            db::messages_schema(),
            &[
                Cell::Str(&self_id),
                Cell::Str(sender),
                Cell::Str(recipient),
                Cell::Str("self"),
                Cell::Str(self_ct),
                Cell::Str(self_iv),
                Cell::Str(self_spk),
                Cell::F64(ts),
                Cell::Str(msg_type),
                Cell::Str(""),
            ],
        )
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        db_ops::append(&messages, row)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    }

    let relay = json!({
        "type": "dm",
        "id": msg_id,
        "sender": sender,
        "recipient": recipient,
        "channel_id": "",
        "ciphertext": ciphertext,
        "iv": iv,
        "sender_public_key_jwk": spk,
        "timestamp": ts,
        "message_type": msg_type,
    });
    let relay_text = relay.to_string();
    state.ws_connections.send_to(recipient, &relay_text);
    state
        .ws_connections
        .send_to_except(sender, own_id, &relay_text);

    let mut ack = relay;
    ack["ack"] = Value::String(msg_id);
    let _ = own_tx.send(text_msg(&ack));
    Ok(())
}

async fn handle_channel(
    state: &AppState,
    sender: &str,
    _own_id: Uuid,
    own_tx: &mpsc::UnboundedSender<Message>,
    data: Value,
) -> anyhow::Result<()> {
    // Parse the payload via the same model the REST handler uses.
    let payload: ChannelMessagePayload = match serde_json::from_value(data) {
        Ok(p) => p,
        Err(e) => {
            let _ = own_tx.send(text_msg(&json!({"error": format!("Invalid channel payload: {e}")})));
            return Ok(());
        }
    };

    // Verify membership; on rejection, send {"error": "..."} like the Python server.
    let members_table = db_ops::open(&state.conn, "channel_members")
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mem_rows = db_ops::scan_where(
        &members_table,
        &format!(
            "channel_id = '{}' AND username = '{sender}'",
            payload.channel_id
        ),
        Some(1),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    if db_ops::total_rows(&mem_rows) == 0 {
        let _ = own_tx.send(text_msg(&json!({"error": "Not a member of this channel"})));
        return Ok(());
    }

    let result = store_channel_message(state, sender, &payload.channel_id, &payload)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    relay_channel_message(state, sender, &payload, &result);

    let ack = json!({
        "ack": result.group_id,
        "type": "channel",
        "channel_id": payload.channel_id,
        "sender": sender,
        "timestamp": result.timestamp,
    });
    let _ = own_tx.send(text_msg(&ack));
    Ok(())
}

fn text_msg(v: &Value) -> Message {
    Message::Text(Utf8Bytes::from(v.to_string()))
}
