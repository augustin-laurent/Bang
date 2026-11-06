//! Bridge from the Slint interface to the per-user mail service.

use crate::{state::UiState, BangApp};
use bang_core::{
    events::{MailEvent, UiCommand},
    ipc::{ClientMessage, ServerMessage, VERSION},
    store::Store,
};
use slint::{invoke_from_event_loop, ComponentHandle};
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt},
            net::UnixStream,
        },
    },
    sync::{mpsc, Arc},
    time::Duration,
};

const MAX_FRAME: usize = 64 * 1024 * 1024;

fn validate_peer(socket: &std::path::Path, stream: &UnixStream) -> anyhow::Result<()> {
    let directory = std::fs::symlink_metadata(
        socket
            .parent()
            .ok_or_else(|| anyhow::anyhow!("invalid socket path"))?,
    )?;
    let runtime = std::fs::symlink_metadata(
        socket
            .parent()
            .and_then(|parent| parent.parent())
            .ok_or_else(|| anyhow::anyhow!("invalid runtime directory"))?,
    )?;
    let file = std::fs::symlink_metadata(socket)?;
    let uid = unsafe { libc::geteuid() };
    anyhow::ensure!(
        runtime.is_dir()
            && !runtime.file_type().is_symlink()
            && runtime.uid() == uid
            && runtime.mode() & 0o077 == 0,
        "unsafe runtime directory"
    );
    anyhow::ensure!(
        directory.is_dir()
            && !directory.file_type().is_symlink()
            && directory.uid() == uid
            && directory.mode() & 0o077 == 0,
        "unsafe service directory"
    );
    anyhow::ensure!(
        file.file_type().is_socket() && file.uid() == uid && file.mode() & 0o077 == 0,
        "unsafe service socket"
    );
    let mut peer = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            peer.as_mut_ptr().cast(),
            &mut len,
        )
    };
    anyhow::ensure!(
        result == 0 && len as usize == std::mem::size_of::<libc::ucred>(),
        "cannot verify service identity"
    );
    anyhow::ensure!(
        unsafe { peer.assume_init().uid } == uid,
        "service has a different owner"
    );
    Ok(())
}

fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> anyhow::Result<()> {
    let data = serde_json::to_vec(value)?;
    anyhow::ensure!(data.len() <= MAX_FRAME, "frame too large");
    stream.write_all(&(data.len() as u32).to_be_bytes())?;
    stream.write_all(&data)?;
    Ok(())
}

fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> anyhow::Result<T> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    anyhow::ensure!(len > 0 && len <= MAX_FRAME, "invalid frame length");
    let mut data = vec![0; len];
    stream.read_exact(&mut data)?;
    Ok(serde_json::from_slice(&data)?)
}

fn connect_service() -> anyhow::Result<UnixStream> {
    let socket = bang_core::ipc::socket_path()?;
    if let Ok(stream) = UnixStream::connect(&socket) {
        validate_peer(&socket, &stream)?;
        return Ok(stream);
    }
    let service = std::env::current_exe()?.with_file_name("bang-service");
    let mut child = std::process::Command::new(service)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Reap a service that exits while the interface remains open; dropping a
    // Child handle alone would leave a zombie until the interface exits.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => {
                validate_peer(&socket, &stream)?;
                return Ok(stream);
            }
            Err(error) if std::time::Instant::now() >= deadline => return Err(error.into()),
            Err(_) => std::thread::sleep(Duration::from_millis(40)),
        }
    }
}

/// Handle to the mail engine running in the per-user service.
#[derive(Clone)]
pub struct Bridge {
    cmd_tx: mpsc::Sender<(ClientMessage, Option<mpsc::SyncSender<()>>)>,
}

impl Bridge {
    /// Send a command to the mail service.
    pub fn send(&self, cmd: UiCommand) {
        let _ = self.cmd_tx.send((ClientMessage::Command(cmd), None));
    }

    /// Refresh the service's view of the persisted OTP configuration.
    pub fn reload_otp_settings(&self) {
        let _ = self.cmd_tx.send((ClientMessage::ReloadOtpSettings, None));
    }

    /// Request a clean stop of the user service.
    pub fn shutdown(&self) {
        let (ack, received) = mpsc::sync_channel(0);
        if self
            .cmd_tx
            .send((ClientMessage::Shutdown, Some(ack)))
            .is_ok()
        {
            let _ = received.recv_timeout(Duration::from_secs(1));
        }
    }

    /// Connect to the service and forward its events to the UI thread.
    pub fn start(
        ui: &BangApp,
        store: Arc<Store>,
        state: Arc<UiState>,
    ) -> Result<Self, slint::PlatformError> {
        let mut stream = connect_service()
            .map_err(|e| slint::PlatformError::Other(format!("Cannot start Bang service: {e}")))?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        write_frame(&mut stream, &ClientMessage::Hello { version: VERSION })
            .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
        let reply = read_frame::<ServerMessage>(&mut stream)
            .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
        if !matches!(reply, ServerMessage::Ready { version: VERSION }) {
            return Err(slint::PlatformError::Other(
                "Incompatible Bang service".into(),
            ));
        }
        stream.set_read_timeout(None).ok();
        let mut outgoing = stream
            .try_clone()
            .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
        let (tx, rx) = mpsc::channel::<(ClientMessage, Option<mpsc::SyncSender<()>>)>();
        std::thread::Builder::new()
            .name("bang-service-writer".into())
            .spawn(move || {
                for (command, ack) in rx {
                    if write_frame(&mut outgoing, &command).is_err() {
                        break;
                    }
                    if let Some(ack) = ack {
                        let _ = ack.send(());
                    }
                }
            })
            .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
        let weak = ui.as_weak();
        std::thread::Builder::new().name("bang-service-reader".into()).spawn(move || {
            loop {
                let event = match read_frame::<ServerMessage>(&mut stream) {
                    Ok(ServerMessage::Event(event)) => event,
                    Ok(ServerMessage::OtpState { enabled }) => {
                        let weak = weak.clone();
                        let _ = invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() { ui.set_settings_otp(enabled); }
                        });
                        continue;
                    },
                    Ok(ServerMessage::Error { message }) => MailEvent::Error { account: None, message },
                    Ok(ServerMessage::Ready { .. }) => continue,
                    Err(_) => MailEvent::Error { account: None, message: "Bang service disconnected".into() },
                };
                let disconnected = matches!(&event, MailEvent::Error { message, .. } if message == "Bang service disconnected");
                let weak = weak.clone(); let store = store.clone(); let state = state.clone();
                let _ = invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() { state.apply(&ui, &store, event); }
                });
                if disconnected { break; }
            }
        }).map_err(|e| slint::PlatformError::Other(e.to_string()))?;
        Ok(Self { cmd_tx: tx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};

    #[test]
    fn refuses_a_non_private_service_socket() {
        let root = std::env::temp_dir().join(format!("bang-peer-test-{}", std::process::id()));
        let dir = root.join("bang");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("service.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let stream = UnixStream::connect(&path).unwrap();
        assert!(validate_peer(&path, &stream).is_ok());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(validate_peer(&path, &stream).is_err());
        drop(stream);
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}
