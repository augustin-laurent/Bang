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
                    while let Some(cmd) = cmd_rx.recv().await {
                        service.handle_command(cmd);
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
