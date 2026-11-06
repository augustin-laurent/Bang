//! Versioned local protocol used by the desktop client and its mail service.

use serde::{Deserialize, Serialize};

use crate::events::{MailEvent, UiCommand};

/// Current version of the local service protocol.
pub const VERSION: u32 = 2;

/// A request sent to the service over a private Unix-domain socket.
#[derive(Serialize, Deserialize)]
pub enum ClientMessage {
    /// Identify the client and negotiate the protocol before issuing commands.
    Hello {
        /// Requested protocol version.
        version: u32,
    },
    /// Execute an existing mail operation.
    Command(UiCommand),
    /// Reload the persisted OTP configuration and return its effective state.
    ReloadOtpSettings,
    /// Stop the service and all account workers.
    Shutdown,
}

/// A response or event delivered to an attached interface.
#[derive(Serialize, Deserialize)]
pub enum ServerMessage {
    /// Handshake succeeded.
    Ready {
        /// Negotiated protocol version.
        version: u32,
    },
    /// A mail event emitted by the engine.
    Event(MailEvent),
    /// Current effective OTP suggestion setting in the service.
    OtpState {
        /// Whether recent verification messages are inspected.
        enabled: bool,
    },
    /// The request could not be accepted.
    Error {
        /// Human-readable error description.
        message: String,
    },
}

/// Resolve the private socket location in the current user's runtime directory.
pub fn socket_path() -> std::io::Result<std::path::PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR is not set"))?;
    let runtime = std::path::PathBuf::from(runtime);
    if !runtime.is_absolute() {
        return Err(std::io::Error::other("XDG_RUNTIME_DIR must be absolute"));
    }
    Ok(runtime.join("bang").join("service.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_round_trip_keeps_version_and_command() {
        let request = ClientMessage::Command(UiCommand::SelectFolder {
            account: crate::account::AccountId("local".into()),
            folder_id: 42,
        });
        let encoded = serde_json::to_vec(&request).unwrap();
        assert!(matches!(
            serde_json::from_slice::<ClientMessage>(&encoded).unwrap(),
            ClientMessage::Command(UiCommand::SelectFolder { folder_id: 42, .. })
        ));
        let encoded = serde_json::to_vec(&ServerMessage::Ready { version: VERSION }).unwrap();
        assert!(matches!(
            serde_json::from_slice::<ServerMessage>(&encoded).unwrap(),
            ServerMessage::Ready { version: VERSION }
        ));
        let encoded = serde_json::to_vec(&ClientMessage::ReloadOtpSettings).unwrap();
        assert!(matches!(
            serde_json::from_slice::<ClientMessage>(&encoded).unwrap(),
            ClientMessage::ReloadOtpSettings
        ));
        let encoded = serde_json::to_vec(&ServerMessage::OtpState { enabled: true }).unwrap();
        assert!(matches!(
            serde_json::from_slice::<ServerMessage>(&encoded).unwrap(),
            ServerMessage::OtpState { enabled: true }
        ));
    }
}
