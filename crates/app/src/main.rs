//! Bang desktop mail client — entry point.

use std::sync::Arc;

use slint::include_modules;

include_modules!();

mod bridge;
mod state;

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let dirs = directories::ProjectDirs::from("", "", "bang")
        .ok_or_else(|| slint::PlatformError::Other("no data directory".into()))?;
    std::fs::create_dir_all(dirs.data_dir()).ok();
    let store = Arc::new(
        bang_core::Store::open(&dirs.data_dir().join("bang.db"))
            .map_err(|e| slint::PlatformError::Other(e.to_string()))?,
    );

    let ui = BangApp::new()?;
    let state = Arc::new(state::UiState::new());
    let bridge = bridge::Bridge::start(&ui, store.clone(), state.clone());

    // Resume syncing any saved accounts.
    if let Ok(accounts) = store.get_accounts() {
        for account in accounts {
            bridge.send(bang_core::UiCommand::AddAccount(account));
        }
    }

    // When the window closes, `run` returns and the process exits, which
    // terminates the syncer threads (IMAP servers notice the TCP drop).
    // `bridge` must stay alive until then: it owns the command channel.
    ui.run()
}
