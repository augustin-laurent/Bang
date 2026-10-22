//! Integration tests: the sync engine against a minimal in-process IMAP server.
//!
//! Run with `cargo test -p bang-core --features integration`. The server
//! speaks just enough IMAP4rev1 (LOGIN, LIST, SELECT, UID FETCH, UID STORE,
//! IDLE, LOGOUT) to exercise the syncer's real wire path, including IDLE
//! push notifications.

#![cfg(feature = "integration")]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use bang_core::account::{AccountId, AccountConfig, AuthMethod, ImapConfig, SmtpConfig, TlsMode};
use bang_core::events::MailEvent;
use bang_core::message::{AccountStatus, MessageFlags};
use bang_core::store::Store;
use bang_core::sync::{start_syncer, SyncerCommand};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

// ---------------------------------------------------------------------------
// Minimal IMAP server
// ---------------------------------------------------------------------------

/// One message held by the test server.
#[derive(Clone)]
struct TestMsg {
    uid: u32,
    flags: MessageFlags,
    subject: String,
    from_name: String,
    from_addr: String,
    date: i64,
    raw: Vec<u8>,
}

/// Shared server state: the folders and their messages.
#[derive(Default)]
struct ServerState {
    folders: Vec<(String, Vec<TestMsg>)>,
    commands: Vec<String>,
    uid_validity: u32,
}

/// A minimal single-user IMAP server bound to 127.0.0.1.
struct MiniImap {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<ServerState>>,
    push_flag: Arc<AtomicBool>,
    _thread: JoinHandle<()>,
}

impl MiniImap {
    /// Start the server with the given folders.
    fn start(folders: Vec<(String, Vec<TestMsg>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test port");
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(ServerState { folders, commands: Vec::new(), uid_validity: 1 }));
        let push_flag = Arc::new(AtomicBool::new(false));
        let thread_state = state.clone();
        let thread_push = push_flag.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(stream) = stream {
                    let state = thread_state.clone();
                    let push_flag = thread_push.clone();
                    std::thread::spawn(move || {
                        let _ = handle_conn(stream, state, push_flag);
                    });
                }
            }
        });
        Self {
            addr,
            state,
            push_flag,
            _thread: thread,
        }
    }

    /// Append a message to INBOX and wake any idling client.
    fn push_inbox(&self, msg: TestMsg) {
        {
            let mut st = self.state.lock().unwrap();
            if let Some((_, msgs)) = st.folders.iter_mut().find(|(n, _)| n == "INBOX") {
                msgs.push(msg);
            }
        }
        self.push_flag.store(true, Ordering::SeqCst);
    }
}

fn write_line(w: &mut TcpStream, line: &str) {
    w.write_all(format!("{line}\r\n").as_bytes()).ok();
    w.flush().ok();
}

/// IMAP flag list for a `MessageFlags` value (space-separated).
fn imap_flags(flags: MessageFlags) -> String {
    let mut parts = Vec::new();
    if flags.contains(MessageFlags::SEEN) {
        parts.push("\\Seen");
    }
    if flags.contains(MessageFlags::FLAGGED) {
        parts.push("\\Flagged");
    }
    if flags.contains(MessageFlags::ANSWERED) {
        parts.push("\\Answered");
    }
    if flags.contains(MessageFlags::DELETED) {
        parts.push("\\Deleted");
    }
    if flags.contains(MessageFlags::DRAFT) {
        parts.push("\\Draft");
    }
    parts.join(" ")
}

/// Parse a quoted or bare IMAP string argument.
fn unquote(s: &str) -> &str {
    let t = s.trim();
    t.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(t)
}

/// Expand a UID set (`N`, `N:*`, `N:M`) against the folder's UIDs.
fn expand_uid_set(set: &str, msgs: &[TestMsg]) -> Vec<u32> {
    if set.contains(',') {
        return set.split(',').flat_map(|part| expand_uid_set(part, msgs)).collect();
    }
    let uids: Vec<u32> = msgs.iter().map(|m| m.uid).collect();
    let (lo_s, hi_s) = match set.split_once(':') {
        Some((a, b)) => (a, b),
        None => (set, set),
    };
    let lo: u32 = lo_s.parse().unwrap_or(1);
    let hi: u32 = if hi_s == "*" {
        uids.last().copied().unwrap_or(lo)
    } else {
        hi_s.parse().unwrap_or(lo)
    };
    uids.into_iter().filter(|u| *u >= lo && *u <= hi).collect()
}

/// Handle one client connection until LOGOUT or EOF.
fn handle_conn(
    stream: TcpStream,
    state: Arc<Mutex<ServerState>>,
    push_flag: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    write_line(&mut writer, "* OK Bang test IMAP server ready");

    let mut selected: Option<String> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let (tag, rest) = match line.split_once(' ') {
            Some(t) => t,
            None => continue,
        };
        state.lock().unwrap().commands.push(rest.trim().into());
        let upper = rest.to_ascii_uppercase();

        if upper.starts_with("LOGIN") {
            write_line(&mut writer, &format!("{tag} OK [PREAUTH] LOGIN completed"));
        } else if upper.starts_with("CAPABILITY") {
            write_line(&mut writer, "* CAPABILITY IMAP4rev1 IDLE");
            write_line(&mut writer, &format!("{tag} OK CAPABILITY completed"));
        } else if upper.starts_with("LIST") {
            let st = state.lock().unwrap();
            for (name, _) in &st.folders {
                write_line(
                    &mut writer,
                    &format!("* LIST (\\HasNoChildren) \"/\" \"{name}\""),
                );
            }
            write_line(&mut writer, &format!("{tag} OK LIST completed"));
        } else if upper.starts_with("SELECT") {
            let name = unquote(rest.trim().strip_prefix("SELECT").unwrap_or("")).to_string();
            let msgs = {
                let st = state.lock().unwrap();
                st.folders
                    .iter()
                    .find(|(n, _)| n == &name)
                    .map(|(_, m)| m.to_vec())
                    .unwrap_or_default()
            };
            selected = Some(name.clone());
            write_line(&mut writer, &format!("* {} EXISTS", msgs.len()));
            write_line(
                &mut writer,
                "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)",
            );
            let validity = state.lock().unwrap().uid_validity;
            write_line(&mut writer, &format!("* OK [UIDVALIDITY {validity}] UIDs valid"));
            let uid_next = msgs.last().map(|m| m.uid + 1).unwrap_or(1);
            write_line(&mut writer, &format!("* OK [UIDNEXT {uid_next}] Predicted next UID"));
            write_line(&mut writer, &format!("{tag} OK [READ-WRITE] SELECT completed"));
        } else if upper.starts_with("FETCH") {
            // Translate sequence numbers into the corresponding message UIDs.
            let name = selected.clone().unwrap_or_default();
            let args = rest.trim().strip_prefix("FETCH").unwrap().trim();
            let (set, query) = args.split_once(' ').unwrap();
            let st = state.lock().unwrap();
            let msgs = &st.folders.iter().find(|(n, _)| *n == name).unwrap().1;
            let sequence_msgs: Vec<_> = msgs.iter().enumerate().map(|(i, m)| {
                let mut m = m.clone(); m.uid = i as u32 + 1; m
            }).collect();
            let uids = expand_uid_set(set, &sequence_msgs).iter()
                .map(|seq| msgs[*seq as usize - 1].uid.to_string())
                .collect::<Vec<_>>().join(",");
            drop(st);
            handle_uid_fetch(&mut writer, tag, &state, &selected, &format!("UID FETCH {uids} {query}"))?;
        } else if upper.starts_with("UID FETCH") {
            handle_uid_fetch(&mut writer, tag, &state, &selected, rest)?;
        } else if upper.starts_with("UID STORE") {
            handle_uid_store(&mut writer, tag, &state, &selected, rest);
        } else if upper.starts_with("UID EXPUNGE") {
            handle_uid_expunge(&mut writer, tag, &state, &selected, rest);
        } else if upper.starts_with("IDLE") {
            write_line(&mut writer, "+ idling");
            writer.set_read_timeout(Some(Duration::from_millis(200))).ok();
            let done = idle_loop(&mut reader, &mut writer, tag, &state, &selected, &push_flag);
            writer.set_read_timeout(None).ok();
            if !done {
                return Ok(());
            }
        } else if upper.starts_with("CLOSE") {
            write_line(&mut writer, &format!("{tag} OK CLOSE completed"));
        } else if upper.starts_with("NOOP") {
            write_line(&mut writer, &format!("{tag} OK NOOP completed"));
        } else if upper.starts_with("LOGOUT") {
            write_line(&mut writer, "* BYE");
            write_line(&mut writer, &format!("{tag} OK LOGOUT completed"));
            return Ok(());
        } else {
            write_line(&mut writer, &format!("{tag} BAD unknown command"));
        }
    }
}

/// Wait during IDLE for DONE or a push signal; return `false` on disconnect.
fn idle_loop(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    tag: &str,
    state: &Arc<Mutex<ServerState>>,
    selected: &Option<String>,
    push_flag: &Arc<AtomicBool>,
) -> bool {
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return false,
            Ok(_) => {
                if line.trim().eq_ignore_ascii_case("DONE") {
                    write_line(writer, &format!("{tag} OK IDLE terminated"));
                    return true;
                }
                // Any other command during IDLE is a protocol error for our
                // test client; ignore and keep waiting.
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                if push_flag.swap(false, Ordering::SeqCst) {
                    let name = selected.clone().unwrap_or_default();
                    let count = state
                        .lock()
                        .unwrap()
                        .folders
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, m)| m.len())
                        .unwrap_or(0);
                    write_line(writer, &format!("* {count} EXISTS"));
                }
            }
            Err(_) => return false,
        }
    }
}

/// Respond to `UID FETCH <set> (<query>)`.
fn handle_uid_fetch(
    writer: &mut TcpStream,
    tag: &str,
    state: &Arc<Mutex<ServerState>>,
    selected: &Option<String>,
    rest: &str,
) -> std::io::Result<()> {
    let args = rest.trim().strip_prefix("UID FETCH").unwrap_or("").trim();
    let (set, query) = match args.split_once(' ') {
        Some(t) => t,
        None => {
            write_line(writer, &format!("{tag} BAD malformed FETCH"));
            return Ok(());
        }
    };
    let name = selected.clone().unwrap_or_default();
    let msgs = {
        let st = state.lock().unwrap();
        st.folders
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, m)| m.to_vec())
            .unwrap_or_default()
    };

    let uids = expand_uid_set(set, &msgs);
    let is_body = query.to_ascii_uppercase().contains("BODY.PEEK[]");

    for uid in uids {
        let Some((idx, msg)) = msgs.iter().enumerate().find(|(_, m)| m.uid == uid) else {
            continue;
        };
        let seq = idx + 1;
        if is_body {
            // Literal: `{n}\r\n` then exactly n bytes, then `)`.
            writer
                .write_all(
                    format!("* {seq} FETCH (UID {uid} BODY[] {{{}}}", msg.raw.len()).as_bytes(),
                )
                .and_then(|_| writer.write_all(b"\r\n"))
                .and_then(|_| writer.write_all(&msg.raw))
                .and_then(|_| writer.write_all(b")\r\n"))?;
        } else if query.trim() == "(UID FLAGS)" {
            write_line(writer, &format!("* {seq} FETCH (UID {uid} FLAGS ({}))", imap_flags(msg.flags)));
        } else {
            let date = chrono::DateTime::from_timestamp(msg.date, 0)
                .map(|d| d.format("%d-%b-%Y %H:%M:%S %z").to_string())
                .unwrap_or_default();
            // imap-proto envelope order: date, subject, from, sender,
            // reply-to, to, cc, bcc, in-reply-to, message-id.
            let envelope = format!(
                "(\"{}\" \"{}\" ((\"{}\" NIL \"{}\" \"bang.test\")) NIL NIL ((\"{}\" NIL \"{}\" \"bang.test\")) NIL NIL NIL \"<{}@bang.test>\")",
                date, msg.subject,
                msg.from_name, msg.from_addr,
                "Bob", "bob",
                msg.from_addr,
            );
            write_line(
                writer,
                &format!(
                    "* {seq} FETCH (UID {uid} FLAGS ({}) INTERNALDATE \"{date}\" ENVELOPE {envelope} RFC822.SIZE {})",
                    imap_flags(msg.flags),
                    msg.raw.len(),
                ),
            );
        }
    }
    write_line(writer, &format!("{tag} OK UID FETCH completed"));
    Ok(())
}

/// Respond to `UID STORE <set> +FLAGS (...)` / `-FLAGS (...)`.
fn handle_uid_store(
    writer: &mut TcpStream,
    tag: &str,
    state: &Arc<Mutex<ServerState>>,
    selected: &Option<String>,
    rest: &str,
) {
    let args = rest.trim().strip_prefix("UID STORE").unwrap_or("").trim();
    let (set, spec) = match args.split_once(' ') {
        Some(t) => t,
        None => {
            write_line(writer, &format!("{tag} BAD malformed STORE"));
            return;
        }
    };
    let add = spec.starts_with('+');
    let flag_str = spec
        .trim_start_matches(['+', '-'])
        .trim()
        .trim_start_matches("FLAGS")
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')');
    let mut flags = MessageFlags::empty();
    for f in flag_str.split_whitespace() {
        flags |= match f {
            "\\Seen" => MessageFlags::SEEN,
            "\\Flagged" => MessageFlags::FLAGGED,
            "\\Answered" => MessageFlags::ANSWERED,
            "\\Deleted" => MessageFlags::DELETED,
            "\\Draft" => MessageFlags::DRAFT,
            _ => MessageFlags::empty(),
        };
    }

    let name = selected.clone().unwrap_or_default();
    let mut st = state.lock().unwrap();
    if let Some((_, msgs)) = st.folders.iter_mut().find(|(n, _)| *n == name) {
        let uids = expand_uid_set(set, msgs);
        for m in msgs.iter_mut() {
            if uids.contains(&m.uid) {
                if add {
                    m.flags |= flags;
                } else {
                    m.flags -= flags;
                }
            }
        }
        for (idx, m) in msgs.iter().enumerate() {
            write_line(
                writer,
                &format!("* {} FETCH (FLAGS ({})", idx + 1, imap_flags(m.flags)),
            );
        }
    }
    drop(st);
    write_line(writer, &format!("{tag} OK UID STORE completed"));
}

/// Respond to `UID EXPUNGE <set>`: remove the messages from the folder.
fn handle_uid_expunge(
    writer: &mut TcpStream,
    tag: &str,
    state: &Arc<Mutex<ServerState>>,
    selected: &Option<String>,
    rest: &str,
) {
    let set = rest.trim().strip_prefix("UID EXPUNGE").unwrap_or("").trim();
    let name = selected.clone().unwrap_or_default();
    let mut st = state.lock().unwrap();
    if let Some((_, msgs)) = st.folders.iter_mut().find(|(n, _)| *n == name) {
        let uids = expand_uid_set(set, msgs);
        msgs.retain(|m| !uids.contains(&m.uid));
    }
    drop(st);
    write_line(writer, &format!("{tag} OK UID EXPUNGE completed"));
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn test_account(addr: std::net::SocketAddr) -> AccountConfig {
    AccountConfig {
        id: AccountId("test@bang.test".into()),
        email: "test@bang.test".into(),
        imap: ImapConfig {
            host: "127.0.0.1".into(),
            port: addr.port(),
            tls: TlsMode::Plaintext,
        },
        smtp: SmtpConfig {
            host: "127.0.0.1".into(),
            port: addr.port(),
            tls: TlsMode::Plaintext,
        },
        auth: AuthMethod::Password {
            username: "test@bang.test".into(),
            password: "secret".into(),
        },
    }
}

fn msg(uid: u32, subject: &str, flags: MessageFlags) -> TestMsg {
    TestMsg {
        uid,
        flags,
        subject: subject.into(),
        from_name: "Alice".into(),
        from_addr: "alice".into(),
        date: 1_700_000_000,
        raw: format!("From: alice@bang.test\r\nSubject: {subject}\r\n\r\nBody of {subject}\r\n")
            .into_bytes(),
    }
}

/// Wait up to `timeout` for an event matching `pred` (polling `try_recv`).
fn wait_for_event(
    rx: &mut UnboundedReceiver<MailEvent>,
    timeout: Duration,
    pred: impl Fn(&MailEvent) -> bool,
) -> Option<MailEvent> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        loop {
            match rx.try_recv() {
                Ok(ev) if pred(&ev) => return Some(ev),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Start a syncer against the server and wait until it reports Connected.
fn start_and_connect(
    server: &MiniImap,
    store: Arc<Store>,
) -> (
    mpsc::Sender<SyncerCommand>,
    UnboundedReceiver<MailEvent>,
) {
    let config = test_account(server.addr);
    // The syncer assumes the account already exists in the store (the app's
    // MailService saves it before starting the syncer).
    store.save_account(&config).unwrap();
    let (events_tx, rx) = unbounded_channel();
    let cmd_tx = start_syncer(config, store, events_tx);
    let mut rx = rx;
    assert!(
        wait_for_event(&mut rx, Duration::from_secs(10), |e| matches!(
            e,
            MailEvent::AccountStatus {
                status: AccountStatus::Connected,
                ..
            }
        ))
        .is_some(),
        "syncer did not reach Connected"
    );
    (cmd_tx, rx)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn initial_sync_populates_store() {
    let server = MiniImap::start(vec![
        (
            "INBOX".into(),
            vec![
                msg(1, "First message", MessageFlags::empty()),
                msg(2, "Second message", MessageFlags::SEEN),
            ],
        ),
        ("Sent".into(), vec![msg(1, "Sent message", MessageFlags::SEEN)]),
    ]);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let (cmd_tx, mut rx) = start_and_connect(&server, store.clone());

    // Folders landed in the store.
    let folders = store.get_folders("test@bang.test").unwrap();
    assert_eq!(folders.len(), 2);
    assert!(folders.iter().any(|f| f.path == "INBOX"));
    assert!(folders.iter().any(|f| f.path == "Sent"));

    // INBOX messages landed with the right subjects and flags.
    let inbox = folders.iter().find(|f| f.path == "INBOX").unwrap();
    let messages = store.get_messages(inbox.id, 100).unwrap();
    assert_eq!(messages.len(), 2);
    let subjects: Vec<&str> = messages.iter().map(|m| m.subject.as_str()).collect();
    assert!(subjects.contains(&"First message"));
    assert!(subjects.contains(&"Second message"));
    let second = messages.iter().find(|m| m.subject == "Second message").unwrap();
    assert!(second.flags.contains(MessageFlags::SEEN));

    // A FoldersUpdated event was emitted.
    assert!(
        wait_for_event(&mut rx, Duration::from_secs(5), |e| {
            matches!(e, MailEvent::FoldersUpdated { .. })
        })
        .is_some()
    );

    cmd_tx.send(SyncerCommand::Quit).unwrap();
}

#[test]
fn idle_push_delivers_new_message() {
    let server = MiniImap::start(vec![(
        "INBOX".into(),
        vec![msg(1, "Initial", MessageFlags::empty())],
    )]);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let (cmd_tx, mut rx) = start_and_connect(&server, store.clone());

    // Let the syncer settle into IDLE.
    std::thread::sleep(Duration::from_millis(500));

    // Push a new message; the server wakes the idling client.
    server.push_inbox(msg(2, "Pushed message", MessageFlags::empty()));

    // The syncer re-syncs and appends the new message.
    let inbox = store.get_folders("test@bang.test").unwrap()[0].clone();
    let got = wait_for_event(&mut rx, Duration::from_secs(10), |e| {
        matches!(e, MailEvent::MessagesAppended { .. })
            && store
                .get_messages(inbox.id, 100)
                .map(|ms| ms.iter().any(|m| m.subject == "Pushed message"))
                .unwrap_or(false)
    });
    assert!(got.is_some(), "pushed message was not delivered");

    let messages = store.get_messages(inbox.id, 100).unwrap();
    assert_eq!(messages.len(), 2);

    cmd_tx.send(SyncerCommand::Quit).unwrap();
}

#[test]
fn delete_removes_message_from_server_and_store() {
    let server = MiniImap::start(vec![(
        "INBOX".into(),
        vec![
            msg(1, "Keep me", MessageFlags::empty()),
            msg(2, "Delete me", MessageFlags::empty()),
        ],
    )]);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let (cmd_tx, mut rx) = start_and_connect(&server, store.clone());

    let inbox = store.get_folders("test@bang.test").unwrap()[0].clone();
    let messages = store.get_messages(inbox.id, 100).unwrap();
    assert_eq!(messages.len(), 2);
    let doomed = messages
        .iter()
        .find(|m| m.subject == "Delete me")
        .unwrap()
        .id;

    cmd_tx
        .send(SyncerCommand::Delete {
            message_ids: vec![doomed],
        })
        .unwrap();

    // The syncer expunges on the server and removes the local row.
    let gone = wait_for_event(&mut rx, Duration::from_secs(10), |e| {
        matches!(e, MailEvent::MessagesUpdated { .. })
            && store.get_message(doomed).map(|m| m.is_none()).unwrap_or(false)
    });
    assert!(gone.is_some(), "deletion was not applied");

    let messages = store.get_messages(inbox.id, 100).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].subject, "Keep me");

    // The server lost the message too.
    let st = server.state.lock().unwrap();
    let server_msgs = st
        .folders
        .iter()
        .find(|(n, _)| n == "INBOX")
        .map(|(_, m)| m.clone())
        .unwrap_or_default();
    assert_eq!(server_msgs.len(), 1);
    assert_eq!(server_msgs[0].subject, "Keep me");

    cmd_tx.send(SyncerCommand::Quit).unwrap();
}


#[test]
fn sync_fetches_metadata_only_and_preserves_lazy_cache_on_flag_refresh() {
    use bang_core::events::UiCommand;
    use bang_core::service::MailService;

    let server = MiniImap::start(vec![(
        "INBOX".into(),
        vec![msg(1, "Original", MessageFlags::empty())],
    )]);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let (tx, mut events) = start_and_connect(&server, store.clone());
    let folder = store.get_folders("test@bang.test").unwrap().remove(0);
    let summary = store.get_messages(folder.id, 10).unwrap().remove(0);
    assert!(store.get_message_full(summary.id).unwrap().is_none());
    assert!(server
        .state
        .lock()
        .unwrap()
        .commands
        .iter()
        .all(|c| !c.contains("BODY")));

    tx.send(SyncerCommand::FetchBody {
        message_id: summary.id,
    })
    .unwrap();
    server.push_flag.store(true, Ordering::SeqCst);
    wait_for_event(&mut events, Duration::from_secs(15), |e| {
        matches!(e, MailEvent::MessageLoaded { .. })
    })
    .expect("lazy body fetch");
    let before = store.get_message_full(summary.id).unwrap().unwrap();
    let checkpoint = server.state.lock().unwrap().commands.len();
    {
        let mut st = server.state.lock().unwrap();
        st.folders[0].1[0].flags = MessageFlags::SEEN | MessageFlags::FLAGGED;
    }
    server.push_inbox(msg(2, "New", MessageFlags::empty()));
    wait_for_event(&mut events, Duration::from_secs(15), |e| matches!(e, MailEvent::MessagesAppended { messages, .. } if messages.iter().any(|m| m.uid == 2))).expect("incremental sync");
    let after = store.get_message_full(summary.id).unwrap().unwrap();
    assert_eq!(after.body_text, before.body_text);
    assert_eq!(after.summary.subject, before.summary.subject);
    assert_eq!(after.summary.preview, before.summary.preview);
    assert_eq!(
        after.summary.flags,
        MessageFlags::SEEN | MessageFlags::FLAGGED
    );
    let commands = server.state.lock().unwrap().commands[checkpoint..].to_vec();
    assert!(commands.iter().any(|c| c == "UID FETCH 1 (UID FLAGS)"));
    assert!(commands
        .iter()
        .any(|c| c == "UID FETCH 2:2 (UID FLAGS INTERNALDATE ENVELOPE RFC822.SIZE)"));
    assert!(commands.iter().all(|c| !c.contains("BODY")));

    // A flags-only update with no new UID must not refetch any envelope.
    let checkpoint = server.state.lock().unwrap().commands.len();
    server.state.lock().unwrap().folders[0].1[0].flags = MessageFlags::empty();
    server.push_flag.store(true, Ordering::SeqCst);
    wait_for_event(&mut events, Duration::from_secs(15), |e| {
        matches!(e, MailEvent::MessagesUpdated { .. })
            && store
                .get_message(summary.id)
                .unwrap()
                .unwrap()
                .flags
                .is_empty()
    })
    .expect("flags-only refresh");
    let commands = server.state.lock().unwrap().commands[checkpoint..].to_vec();
    assert!(commands.iter().any(|c| c.ends_with("(UID FLAGS)")));
    assert!(commands
        .iter()
        .all(|c| !c.contains("ENVELOPE") && !c.contains("BODY")));

    // Cached reopening also works with no account connection.
    let (event_tx, mut cached_events) = unbounded_channel();
    let service = MailService::new(store.clone(), event_tx);
    for _ in 0..2 {
        service.handle_command(UiCommand::OpenMessage {
            account: AccountId("test@bang.test".into()),
            message_id: summary.id,
        });
        assert!(matches!(
            cached_events.try_recv().unwrap(),
            MailEvent::MessageLoaded { .. }
        ));
    }
    tx.send(SyncerCommand::Quit).unwrap();
}

#[test]
fn initial_sync_limits_by_message_count_even_with_sparse_uids() {
    let messages = (1..=250)
        .map(|i| msg(i * 10, &format!("Message {i}"), MessageFlags::empty()))
        .collect();
    let server = MiniImap::start(vec![("INBOX".into(), messages)]);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let (tx, _) = start_and_connect(&server, store.clone());
    let folder = store.get_folders("test@bang.test").unwrap().remove(0);
    let uids = store.recent_message_uids(folder.id, 1000).unwrap();
    assert_eq!(uids.len(), 200);
    assert_eq!(uids.first(), Some(&2500));
    assert_eq!(uids.last(), Some(&510));
    assert!(server
        .state
        .lock()
        .unwrap()
        .commands
        .iter()
        .any(|c| c.starts_with("FETCH 51:250 ")));
    tx.send(SyncerCommand::Quit).unwrap();
}

#[test]
fn uidvalidity_change_discards_old_cached_bodies() {
    let server = MiniImap::start(vec![(
        "INBOX".into(),
        vec![msg(1, "Old", MessageFlags::empty())],
    )]);
    let store = Arc::new(Store::open_in_memory().unwrap());
    let (tx, _) = start_and_connect(&server, store.clone());
    let folder = store.get_folders("test@bang.test").unwrap().remove(0);
    let id = store.get_messages(folder.id, 10).unwrap()[0].id;
    store
        .save_message_bodies(id, "Old cached body", "", &[])
        .unwrap();
    tx.send(SyncerCommand::Quit).unwrap();
    // Wake IDLE and wait for the old syncer to drop its command receiver.
    server.push_flag.store(true, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while tx.send(SyncerCommand::Quit).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "old syncer did not stop"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    {
        let mut st = server.state.lock().unwrap();
        st.uid_validity = 2;
        st.folders[0].1 = vec![msg(1, "Replacement", MessageFlags::empty())];
    }
    let (tx, _) = start_and_connect(&server, store.clone());
    let replacement = store.get_messages(folder.id, 10).unwrap().remove(0);
    assert_eq!(replacement.subject, "Replacement");
    assert!(store.get_message_full(replacement.id).unwrap().is_none());
    assert_eq!(
        store.get_folder_sync_state(folder.id).unwrap(),
        Some((1, Some(2)))
    );
    tx.send(SyncerCommand::Quit).unwrap();
}
