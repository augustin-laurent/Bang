//! Bridge between the tokio runtime (mail core) and the Slint event loop.
//!
//! The core runs on a dedicated OS thread with its own tokio runtime. UI
//! commands cross to it over an unbounded channel; core events cross back
//! over another channel and are applied on the Slint event loop via
//! [`slint::invoke_from_event_loop`].

use std::sync::Arc;

use slint::{invoke_from_event_loop, ComponentHandle, Weak};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use bang_core::events::{MailEvent, UiCommand};
use bang_core::service::MailService;
use bang_core::store::Store;

use crate::state::UiState;
use crate::BangApp;

/// Handle to the mail core running on the background tokio runtime.
#[derive(Clone)]
pub struct Bridge {
    cmd_tx: UnboundedSender<UiCommand>,
}

impl Bridge {
    /// Send a command to the mail core.
    pub fn send(&self, cmd: UiCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Start the core thread and wire its events to the window.
    ///
    /// # Arguments
    /// * `ui` — the main window.
    /// * `store` — the shared local store.
    /// * `state` — the shared UI state.
    pub fn start(ui: &BangApp, store: Arc<Store>, state: Arc<UiState>) -> Self {
        let (cmd_tx, mut cmd_rx) = unbounded_channel::<UiCommand>();
        let (events_tx, events_rx) = unbounded_channel::<MailEvent>();
        // Kept separately so a panic in the command loop can still report.
        let panic_events = events_tx.clone();
        let service = Arc::new(MailService::new(store.clone(), events_tx));
        let weak = ui.as_weak();

        std::thread::Builder::new()
            .name("bang-core".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("bang-tokio")
                    .enable_all()
                    .build()
                    .expect("build tokio runtime");
                rt.block_on(async move {
                    let forward = tokio::spawn(forward_events(weak, state, store, events_rx));
                    // Follow-up reminders are checked periodically. The first
                    // tick fires immediately, so reminders that came due while
                    // the app was closed fire on startup.
                    let mut follow_up_tick =
                        tokio::time::interval(std::time::Duration::from_secs(30));
                    loop {
                        tokio::select! {
                            cmd = cmd_rx.recv() => match cmd {
                                Some(cmd) => {
                                    // A panic must not kill the core thread
                                    // silently: report it and keep serving.
                                    let result = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            service.handle_command(cmd)
                                        }),
                                    );
                                    if let Err(payload) = result {
                                        let message = payload
                                            .downcast_ref::<String>()
                                            .cloned()
                                            .or_else(|| {
                                                payload
                                                    .downcast_ref::<&str>()
                                                    .map(|s| s.to_string())
                                            })
                                            .unwrap_or_else(|| "mail core panicked".into());
                                        tracing::error!("mail core panicked: {message}");
                                        let _ = panic_events.send(MailEvent::Error {
                                            account: None,
                                            message,
                                        });
                                    }
                                }
                                None => break,
                            },
                            _ = follow_up_tick.tick() => {
                                service.check_due_follow_ups();
                                service.check_due_sends();
                            }
                        }
                    }
                    let _ = forward.await;
                });
            })
            .expect("spawn core thread");

        Self { cmd_tx }
    }
}

/// Forward core events to the Slint event loop.
async fn forward_events(
    weak: Weak<BangApp>,
    state: Arc<UiState>,
    store: Arc<Store>,
    mut rx: UnboundedReceiver<MailEvent>,
) {
    while let Some(event) = rx.recv().await {
        let weak = weak.clone();
        let state = state.clone();
        let store = store.clone();
        let _ = invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                state.apply(&ui, &store, event);
            }
        });
    }
}
