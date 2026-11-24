//! Error types for Instant View page preparation, controller, and resource serving.

use thiserror::Error;

/// Errors returned by `tg-iv` operations.
#[derive(Debug, Error)]
pub enum IvError {
    /// Failed to parse a JSON message sent from the WebView JS bridge.
    #[error("invalid bridge message JSON: {0}")]
    InvalidBridgeMessage(String),

    /// The bridge sent an event type or payload that is not recognized.
    #[error("unknown bridge event: {0}")]
    UnknownBridgeEvent(String),

    /// Failed to serialize an outbound host command to JSON.
    #[error("failed to serialize host command: {0}")]
    SerializeCommand(#[from] serde_json::Error),

    /// A requested URL or path was rejected by the security policy.
    #[error("rejected resource path: {0}")]
    RejectedPath(String),
}
