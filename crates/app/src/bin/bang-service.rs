//! Per-user mail engine. The graphical client connects over a private socket.

use bang_core::{
    ipc::{ClientMessage, ServerMessage, VERSION},
    AccountId, AccountStatus, MailEvent, MailService, Store, UiCommand,
};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{broadcast, mpsc, watch},
};

#[path = "service/accessibility.rs"]
mod accessibility;
#[path = "service/notification.rs"]
mod notification;

const MAX_FRAME: usize = 64 * 1024 * 1024;

fn lock_runtime(socket: &Path) -> anyhow::Result<std::fs::File> {
    let dir = socket
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid socket path"))?;
    let runtime = dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid runtime directory"))?;
    let metadata = std::fs::symlink_metadata(runtime)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        anyhow::bail!("XDG_RUNTIME_DIR must be owned by the user and private");
    }
    std::fs::create_dir_all(dir)?;
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        // New directories inherit umask; explicitly make a directory we own private.
        if metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && !metadata.file_type().is_symlink()
        {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        } else {
            anyhow::bail!("unsafe runtime directory");
        }
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("service.lock"))?;
    if unsafe {
        libc::flock(
            std::os::fd::AsRawFd::as_raw_fd(&lock),
            libc::LOCK_EX | libc::LOCK_NB,
        )
    } != 0
    {
        anyhow::bail!("service is already running");
    }
    Ok(lock)
}

fn otp_enabled() -> bool {
    let Some(dirs) = directories::ProjectDirs::from("", "", "bang") else {
        return false;
    };
    std::fs::read_to_string(dirs.config_dir().join("config.toml"))
        .ok()
        .and_then(|contents| toml::from_str::<toml::Value>(&contents).ok())
        .and_then(|config| config.get("otp_enabled").and_then(toml::Value::as_bool))
        .unwrap_or(false)
}

async fn read_frame<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> anyhow::Result<T> {
    let len = reader.read_u32().await? as usize;
    if len == 0 || len > MAX_FRAME {
        anyhow::bail!("invalid frame length");
    }
    let mut data = vec![0; len];
    reader.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}

async fn write_frame<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    value: &T,
) -> anyhow::Result<()> {
    let data = serde_json::to_vec(value)?;
    if data.len() > MAX_FRAME {
        anyhow::bail!("frame too large");
    }
    writer.write_u32(data.len() as u32).await?;
    writer.write_all(&data).await?;
    Ok(())
}

struct Attached(Arc<AtomicUsize>);
impl Drop for Attached {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn client(
    stream: UnixStream,
    service: Arc<MailService>,
    store: Arc<Store>,
    statuses: Arc<Mutex<HashMap<String, AccountStatus>>>,
    events: broadcast::Sender<MailEvent>,
    shutdown: watch::Sender<bool>,
    clients: Arc<AtomicUsize>,
    attached: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        anyhow::bail!("unauthorized peer");
    }
    let (mut reader, mut writer) = stream.into_split();
    let hello: ClientMessage =
        tokio::time::timeout(std::time::Duration::from_secs(5), read_frame(&mut reader)).await??;
    if !matches!(hello, ClientMessage::Hello { version: VERSION }) {
        write_frame(
            &mut writer,
            &ServerMessage::Error {
                message: "Incompatible service protocol".into(),
            },
        )
        .await?;
        return Ok(());
    }
    let mut rx = events.subscribe();
    clients.fetch_add(1, Ordering::SeqCst);
    attached.store(true, Ordering::SeqCst);
    let _attached = Attached(clients);
    write_frame(&mut writer, &ServerMessage::Ready { version: VERSION }).await?;
    // The service may have synchronized while the interface was absent.
    // Supply a snapshot as well as the live event stream after attaching.
    for account in store.get_accounts()? {
        let folders = store.get_folders(&account.id.0)?;
        write_frame(
            &mut writer,
            &ServerMessage::Event(MailEvent::FoldersUpdated {
                account: account.id,
                folders,
            }),
        )
        .await?;
    }
    let snapshot = statuses.lock().unwrap().clone();
    for (id, status) in snapshot {
        write_frame(
            &mut writer,
            &ServerMessage::Event(MailEvent::AccountStatus {
                account: AccountId(id),
                status,
            }),
        )
        .await?;
    }
    // A dedicated reader preserves partially received frames while events are
    // being written. Cancelling read_exact in select! would lose those bytes.
    let (request_tx, mut requests) = mpsc::channel::<ClientMessage>(64);
    let _reader_task = AbortOnDrop(tokio::spawn(async move {
        while let Ok(request) = read_frame::<_, ClientMessage>(&mut reader).await {
            if request_tx.send(request).await.is_err() {
                break;
            }
        }
    }));
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(ClientMessage::Command(UiCommand::RestoreAccounts)) => {}, // already restored by the service
                Some(ClientMessage::Command(command)) => service.handle_command(command),
                Some(ClientMessage::ReloadOtpSettings) => {
                    write_frame(&mut writer, &ServerMessage::OtpState { enabled: otp_enabled() }).await?;
                },
                Some(ClientMessage::Shutdown) => { let _ = shutdown.send(true); break; },
                Some(ClientMessage::Hello { .. }) | None => break,
            },
            event = rx.recv() => match event {
                Ok(event) => write_frame(&mut writer, &ServerMessage::Event(event)).await?,
                Err(broadcast::error::RecvError::Lagged(_)) => anyhow::bail!("client missed mail events"),
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    // The service creates only per-user state, including its IPC socket.
    // Set this before GTK or the runtime start any threads.
    unsafe {
        libc::umask(0o077);
    }
    if !std::env::args_os().any(|arg| arg == "--worker") {
        return supervise();
    }
    run_graphical_worker()
}

// GTK3 can terminate its process directly when the graphical display exits.
// Keep cleanup outside that process so an ended graphical session does not
// leave the private socket behind or make logout look like a service failure.
fn supervise() -> anyhow::Result<()> {
    let parent = std::process::id() as libc::pid_t;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.arg("--worker");
    if std::env::args_os().any(|arg| arg == "--autostart") {
        command.arg("--autostart");
    }
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::other("service supervisor exited"));
            }
            Ok(())
        });
    }
    let status = command.status()?;
    if status.success() {
        return Ok(());
    }
    // A crash can leave the socket behind. Only remove it while holding the
    // same lock as the worker, so a newly started service is never disturbed.
    if let Ok(socket) = bang_core::ipc::socket_path() {
        if let Ok(_lock) = lock_runtime(&socket) {
            let _ = std::fs::remove_file(socket);
        }
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").filter(|_| {
        std::env::var_os("GDK_BACKEND").as_deref() != Some(std::ffi::OsStr::new("x11"))
    });
    if let Some(display) = wayland {
        let display = std::path::PathBuf::from(display);
        let display = if display.is_absolute() {
            display
        } else {
            std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_default())
                .join(display)
        };
        if std::os::unix::net::UnixStream::connect(display).is_err() {
            return Ok(());
        }
    } else if let Ok(display) = std::env::var("DISPLAY") {
        // Local X11 displays use /tmp/.X11-unix/X<number>. Avoid guessing
        // whether remote displays are gone; those still report worker errors.
        let number = display
            .strip_prefix(':')
            .or_else(|| display.strip_prefix("unix:"));
        if let Some(number) = number
            .and_then(|s| s.split('.').next())
            .and_then(|s| s.parse::<u32>().ok())
        {
            let socket = format!("/tmp/.X11-unix/X{number}");
            if std::os::unix::net::UnixStream::connect(socket).is_err() {
                return Ok(());
            }
        }
    }
    anyhow::bail!("Bang service worker exited with {status}")
}

fn run_graphical_worker() -> anyhow::Result<()> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (clipboard_tx, clipboard_rx) = std::sync::mpsc::channel::<String>();
    let graphical = gtk::init().is_ok();
    if !graphical {
        return tokio::runtime::Runtime::new()?.block_on(run(
            shutdown_tx,
            shutdown_rx,
            clipboard_tx,
            false,
        ));
    }
    if let Some(display) = gtk::gdk::Display::default() {
        let shutdown = shutdown_tx.clone();
        display.connect_closed(move |_, _| {
            let _ = shutdown.send(true);
        });
    }
    // GTK owns the X11/Wayland selection for as long as this clipboard object
    // stays alive. Dropping the temporary immediately loses ownership on X11.
    let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
        while let Ok(text) = clipboard_rx.try_recv() {
            clipboard.set_text(&text);
        }
        gtk::glib::ControlFlow::Continue
    });
    let worker = std::thread::Builder::new()
        .name("bang-service-runtime".into())
        .spawn(move || {
            let result = tokio::runtime::Runtime::new().and_then(|rt| {
                rt.block_on(run(shutdown_tx, shutdown_rx, clipboard_tx, true))
                    .map_err(std::io::Error::other)
            });
            gtk::glib::idle_add_once(|| {
                if gtk::main_level() > 0 {
                    gtk::main_quit();
                }
            });
            result
        })?;
    gtk::main();
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("service runtime panicked"))??;
    Ok(())
}

async fn run(
    shutdown_tx: watch::Sender<bool>,
    mut shutdown_rx: watch::Receiver<bool>,
    clipboard_tx: std::sync::mpsc::Sender<String>,
    clipboard_available: bool,
) -> anyhow::Result<()> {
    let socket = bang_core::ipc::socket_path()?;
    let _lock = lock_runtime(&socket)?;
    match std::fs::remove_file(&socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let dirs = directories::ProjectDirs::from("", "", "bang")
        .ok_or_else(|| anyhow::anyhow!("no data directory"))?;
    let store = Arc::new(Store::open(&dirs.data_dir().join("bang.db"))?);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let service = Arc::new(MailService::new(store.clone(), event_tx));
    let (events, _) = broadcast::channel(1024);
    let notifications = notification::start(
        clipboard_tx,
        clipboard_available,
        accessibility::Accessibility::start(),
    );
    let clients = Arc::new(AtomicUsize::new(0));
    let attached = Arc::new(AtomicBool::new(false));
    let autostarted = std::env::args_os().any(|arg| arg == "--autostart");
    let statuses = Arc::new(Mutex::new(HashMap::new()));
    service.handle_command(UiCommand::RestoreAccounts);
    let started = std::time::Instant::now();
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut last_due_check: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let task = client(stream, service.clone(), store.clone(), statuses.clone(), events.clone(), shutdown_tx.clone(), clients.clone(), attached.clone());
                tokio::spawn(async move { let _ = task.await; });
            },
            Some(event) = event_rx.recv() => {
                if let MailEvent::AccountStatus { account, status } = &event {
                    statuses.lock().unwrap().insert(account.0.clone(), status.clone());
                }
                if let MailEvent::AccountRemoved { account, error: None } = &event {
                    statuses.lock().unwrap().remove(&account.0);
                }
                if let MailEvent::OtpDetected { candidate } = event {
                    let still_valid = store.get_message(candidate.message_id).ok().flatten()
                        .is_some_and(|message| message.account_id == candidate.account_id && message.flags.is_unread());
                    if otp_enabled() && still_valid { let _ = notifications.send(candidate); }
                    continue;
                }
                if let MailEvent::MessagesAppended { account, messages, .. } = &event {
                    if otp_enabled() {
                        for message in messages {
                            if message.flags.is_unread() && bang_core::otp::is_fresh(message.date, chrono::Utc::now().timestamp()) {
                                service.scan_otp(account, message.id);
                            }
                        }
                    }
                }
                let _ = events.send(event);
            },
            _ = timer.tick() => {
                if clients.load(Ordering::SeqCst) > 0 {
                    if last_due_check.is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(30)) {
                        service.check_due_follow_ups();
                        service.check_due_sends();
                        last_due_check = Some(std::time::Instant::now());
                    }
                } else {
                    // Reattaching a window must check due work immediately,
                    // as starting the old in-process bridge did.
                    last_due_check = None;
                    if started.elapsed() >= std::time::Duration::from_secs(5) && !otp_enabled()
                        && (!autostarted || attached.load(Ordering::SeqCst)) {
                        break;
                    }
                }
            },
            result = shutdown_rx.changed() => if result.is_err() || *shutdown_rx.borrow() { break; },
        }
    }
    service.handle_command(UiCommand::Quit);
    let _ = std::fs::remove_file(socket);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn framed_protocol_handles_fragmentation_and_rejects_oversize() {
        let (mut left, mut right) = tokio::io::duplex(128);
        let writer = tokio::spawn(async move {
            let payload = serde_json::to_vec(&ClientMessage::Hello { version: VERSION }).unwrap();
            for byte in (payload.len() as u32)
                .to_be_bytes()
                .iter()
                .chain(payload.iter())
            {
                left.write_all(&[*byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        assert!(matches!(
            read_frame::<_, ClientMessage>(&mut right).await.unwrap(),
            ClientMessage::Hello { version: VERSION }
        ));
        writer.await.unwrap();
        let (mut left, mut right) = tokio::io::duplex(8);
        left.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert!(read_frame::<_, ClientMessage>(&mut right).await.is_err());
    }

    #[test]
    fn runtime_lock_allows_only_one_service() {
        let root = std::env::temp_dir().join(format!("bang-service-lock-{}", std::process::id()));
        let socket = root.join("private/service.sock");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = lock_runtime(&socket).unwrap();
        assert!(lock_runtime(&socket).is_err());
        drop(first);
        assert!(lock_runtime(&socket).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
