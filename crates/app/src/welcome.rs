//! First-launch setup and its small persistent lifecycle.
use crate::{config::Config, i18n::tr, BangApp};
use slint::ComponentHandle;
use std::path::PathBuf;

const SAVE_ERROR: &str =
    "Could not save settings. Check access to your configuration folder and try again.";

pub(crate) fn eligible(config: &Config, config_exists: bool, has_accounts: bool) -> bool {
    !has_accounts
        && match config.welcome_completed {
            Some(completed) => !completed,
            None => !config_exists,
        }
}

/// Keep the pending marker even when the initial config write failed.
pub(crate) fn save_preferences(ui: &BangApp, config: &mut Config) -> std::io::Result<()> {
    if ui.get_welcome_open() {
        config.welcome_completed = Some(false);
    }
    let result = config.save();
    if ui.get_welcome_open() {
        ui.set_welcome_error(if result.is_err() {
            tr(SAVE_ERROR).into()
        } else {
            "".into()
        });
    }
    result
}

/// Wire navigation and completion. The path is explicit so fixtures never touch user state.
pub(crate) fn wire(ui: &BangApp, path: PathBuf, has_accounts: bool) {
    let mut config = Config::load_from(&path);
    let open = eligible(&config, path.exists(), has_accounts);
    ui.set_settings_theme(config.theme.clone().into());
    ui.set_settings_language(config.language.clone().into());
    ui.set_welcome_open(open);
    if open {
        config.welcome_completed = Some(false);
        if config.save_to(&path).is_err() {
            ui.set_welcome_error(tr(SAVE_ERROR).into());
        }
    }
    let weak = ui.as_weak();
    ui.on_welcome_add_account(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_wizard_open(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_welcome_finish(move || {
        let Some(ui) = weak.upgrade() else { return };
        if !ui.get_welcome_open() {
            return;
        }
        let mut config = Config::load_from(&path);
        config.welcome_completed = Some(true);
        match config.save_to(&path) {
            Ok(()) => {
                ui.set_welcome_error("".into());
                ui.set_welcome_open(false);
                ui.invoke_focus_chrome();
            }
            Err(_) => {
                ui.set_welcome_step(2);
                ui.set_welcome_error(tr(SAVE_ERROR).into());
                ui.invoke_welcome_focus();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_or_explicitly_pending_installs_show_setup() {
        for (completed, exists, accounts, expected) in [
            (None, false, false, true),
            (None, true, false, false),
            (None, false, true, false),
            (Some(false), true, false, true),
            (Some(false), true, true, false),
            (Some(true), true, false, false),
            (Some(true), false, false, false),
        ] {
            let config = Config {
                welcome_completed: completed,
                ..Config::default()
            };
            assert_eq!(eligible(&config, exists, accounts), expected);
        }
    }
}
