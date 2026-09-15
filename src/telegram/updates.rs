//! Deserialization types for the Telegram Bot API `getUpdates` response.
//! Only the fields Mervyn acts on are modelled; Telegram adds fields freely, so every
//! optional field carries `#[serde(default)]` and unknown fields are ignored.

use serde::Deserialize;

/// One entry from `getUpdates`. `update_id` is the acknowledgement cursor: calling
/// `getUpdates` with an `offset` greater than an `update_id` confirms it server-side.
#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: i64,
    /// Absent for messages sent to a channel.
    #[serde(default)]
    pub from: Option<User>,
    pub chat: Chat,
    /// Absent for non-text messages (photos, stickers, service messages).
    #[serde(default)]
    pub text: Option<String>,
    pub date: i64,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub is_bot: bool,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
}

impl Update {
    /// Sender's numeric id, when the update carries a message from a user.
    pub fn sender_id(&self) -> Option<i64> {
        self.message.as_ref().and_then(|m| m.from.as_ref()).map(|u| u.id)
    }

    /// Chat the reply should go back to.
    pub fn chat_id(&self) -> Option<i64> {
        self.message.as_ref().map(|m| m.chat.id)
    }

    /// Trimmed message text, if this update is a non-empty text message.
    pub fn text(&self) -> Option<&str> {
        self.message
            .as_ref()
            .and_then(|m| m.text.as_deref())
            .map(str::trim)
            .filter(|t| !t.is_empty())
    }

    /// True when the update came from another bot (never process these — loop risk).
    pub fn is_from_bot(&self) -> bool {
        self.message
            .as_ref()
            .and_then(|m| m.from.as_ref())
            .is_some_and(|u| u.is_bot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape captured from a real `getUpdates` response.
    const REAL_UPDATE: &str = r#"{
        "update_id": 884711001,
        "message": {
            "message_id": 42,
            "from": {"id": 12345678, "is_bot": false, "first_name": "Wolf", "language_code": "en"},
            "chat": {"id": 12345678, "first_name": "Wolf", "type": "private"},
            "date": 1757943600,
            "text": "remind me to call the dentist tomorrow"
        }
    }"#;

    #[test]
    fn parses_real_text_update() {
        let u: Update = serde_json::from_str(REAL_UPDATE).unwrap();
        assert_eq!(u.update_id, 884711001);
        assert_eq!(u.sender_id(), Some(12345678));
        assert_eq!(u.chat_id(), Some(12345678));
        assert_eq!(u.text(), Some("remind me to call the dentist tomorrow"));
        assert!(!u.is_from_bot());
    }

    #[test]
    fn unknown_fields_and_missing_optionals_are_tolerated() {
        // No `from`, no `text`, plus a field we do not model.
        let raw = r#"{
            "update_id": 7,
            "message": {
                "message_id": 1,
                "chat": {"id": -100123, "type": "channel"},
                "date": 1757943600,
                "sticker": {"emoji": "x"}
            }
        }"#;
        let u: Update = serde_json::from_str(raw).unwrap();
        assert_eq!(u.sender_id(), None);
        assert_eq!(u.text(), None);
        assert_eq!(u.chat_id(), Some(-100123));
    }

    #[test]
    fn non_message_update_parses_to_none() {
        let raw = r#"{"update_id": 9, "edited_message": {"message_id": 3}}"#;
        let u: Update = serde_json::from_str(raw).unwrap();
        assert!(u.message.is_none());
        assert_eq!(u.text(), None);
    }

    #[test]
    fn whitespace_only_text_is_not_a_message() {
        let raw = r#"{"update_id":1,"message":{"message_id":1,"chat":{"id":5},"date":1,"text":"   \n  "}}"#;
        let u: Update = serde_json::from_str(raw).unwrap();
        assert_eq!(u.text(), None);
    }

    #[test]
    fn bot_sender_is_flagged() {
        let raw = r#"{"update_id":1,"message":{"message_id":1,"from":{"id":9,"is_bot":true},
            "chat":{"id":5},"date":1,"text":"hi"}}"#;
        let u: Update = serde_json::from_str(raw).unwrap();
        assert!(u.is_from_bot());
    }
}
