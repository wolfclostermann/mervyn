//! Telegram Bot API transport: long polling for inbound messages, `sendMessage` for replies.
//!
//! Long polling means Mervyn only makes **outbound** connections — there is no public
//! endpoint, no tunnel and no request-signature check. The trade-off is that the transport
//! no longer authenticates anything: any Telegram user who finds the bot can message it, so
//! the sender allowlist is what keeps it private.

pub mod client;
pub mod updates;
