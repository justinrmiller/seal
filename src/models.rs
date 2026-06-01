use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub public_key_jwk: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub token: String,
    pub username: String,
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct UserListItem {
    pub username: String,
}

#[derive(Debug, Serialize)]
pub struct UserPublicKeyResponse {
    pub username: String,
    pub public_key_jwk: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateChannelRequest {
    pub name: String,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddChannelMemberRequest {
    pub username: String,
}

#[derive(Debug, Serialize)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    pub created_by: String,
    pub members: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ChannelBrowseItem {
    pub id: String,
    pub name: String,
    pub created_by: String,
    pub member_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ChannelMemberKey {
    pub username: String,
    pub public_key_jwk: String,
}

#[derive(Debug, Deserialize)]
pub struct AfterQuery {
    pub token: String,
    #[serde(default)]
    pub after: f64,
}

#[derive(Debug, Deserialize)]
pub struct ChannelEncryptedEnvelope {
    pub target_user: String,
    pub ciphertext: String,
    pub iv: String,
    pub sender_public_key_jwk: String,
}

#[derive(Debug, Deserialize)]
pub struct ChannelAttachment {
    pub encrypted_data: String,
    pub iv: String,
}

#[derive(Debug, Deserialize)]
pub struct ChannelMessagePayload {
    pub channel_id: String,
    pub envelopes: Vec<ChannelEncryptedEnvelope>,
    #[serde(default = "default_message_type")]
    pub message_type: String,
    #[serde(default)]
    pub attachment: Option<ChannelAttachment>,
}

fn default_message_type() -> String {
    "text".into()
}

#[derive(Debug, Serialize)]
pub struct StoredMessage {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub channel_id: String,
    pub ciphertext: String,
    pub iv: String,
    pub sender_public_key_jwk: String,
    pub timestamp: f64,
    pub message_type: String,
    pub attachment_id: String,
}

#[derive(Debug, Serialize)]
pub struct AttachmentResponse {
    pub id: String,
    pub encrypted_data: String,
    pub iv: String,
}
