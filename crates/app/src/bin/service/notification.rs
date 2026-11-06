//! Native notifications and explicit clipboard access for OTP candidates.

use super::accessibility::{Accessibility, Target};
use bang_core::otp::OtpCandidate;
use futures_util::StreamExt;
use gtk::glib;
use std::collections::HashMap;
use tokio::sync::mpsc;

fn translate(key: &'static str) -> &'static str {
    let config = directories::ProjectDirs::from("", "", "bang")
        .and_then(|dirs| std::fs::read_to_string(dirs.config_dir().join("config.toml")).ok())
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok());
    let configured = config
        .as_ref()
        .and_then(|c| c.get("language"))
        .and_then(toml::Value::as_str)
        .unwrap_or("system");
    let locale = if configured == "system" {
        std::env::var("LC_ALL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("LC_MESSAGES").ok())
            .or_else(|| std::env::var("LANG").ok())
            .unwrap_or_default()
    } else {
        configured.to_owned()
    };
    if !locale.to_ascii_lowercase().starts_with("fr") {
        return key;
    }
    match key {
        "Verification code received" => "Code de vérification reçu",
        "Insert code" => "Insérer le code",
        "Copy code" => "Copier le code",
        "Could not insert; copy the code" => "Insertion impossible : copiez le code",
        _ => key,
    }
}

fn notification_body(candidate: &OtpCandidate) -> String {
    // Addresses are untrusted message metadata and may themselves contain
    // the code. Mask it before escaping the strings for desktop markup.
    let sender = candidate.sender.replace(&candidate.code, "••••");
    let recipient = candidate.recipient.replace(&candidate.code, "••••");
    format!(
        "{} → {}",
        glib::markup_escape_text(&sender),
        glib::markup_escape_text(&recipient)
    )
}

/// Start the desktop adapters. Failure to reach the desktop leaves mail usable.
pub(super) fn start(
    clipboard_tx: std::sync::mpsc::Sender<String>,
    clipboard_available: bool,
    accessibility: Accessibility,
) -> mpsc::UnboundedSender<OtpCandidate> {
    let (candidate_tx, mut candidate_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            if let Err(error) = run(
                &mut candidate_rx,
                clipboard_tx.clone(),
                clipboard_available,
                accessibility.clone(),
            )
            .await
            {
                eprintln!("Bang verification notifications unavailable: {error}");
            }
            // Drop codes arriving while the desktop notification service is
            // unavailable; never retain them indefinitely across retries.
            let wait = tokio::time::sleep(std::time::Duration::from_secs(5));
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    item = candidate_rx.recv() => if item.is_none() { return; },
                }
            }
        }
    });
    candidate_tx
}

async fn run(
    candidates: &mut mpsc::UnboundedReceiver<OtpCandidate>,
    clipboard: std::sync::mpsc::Sender<String>,
    clipboard_available: bool,
    accessibility: Accessibility,
) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await?;
    let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await?;
    let actions_supported = capabilities.iter().any(|c| c == "actions");
    let mut actions = proxy.receive_signal("ActionInvoked").await?;
    let mut closed = proxy.receive_signal("NotificationClosed").await?;
    let mut live = HashMap::<u32, (OtpCandidate, Option<Target>)>::new();
    let mut enabled = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let next_expiry = live
            .values()
            .map(|(candidate, _)| {
                candidate
                    .received_at
                    .saturating_add(bang_core::otp::MAX_AGE_SECONDS)
                    .saturating_mul(1000)
                    .saturating_sub(now_ms)
                    .max(0) as u64
            })
            .min();
        tokio::select! {
            Some(candidate) = candidates.recv() => {
                if !bang_core::otp::is_fresh(candidate.received_at, chrono::Utc::now().timestamp()) { continue; }
                let body = notification_body(&candidate);
                let target = accessibility.target().await;
                let mut buttons = Vec::new();
                if actions_supported {
                    if target.is_some() { buttons.extend(["insert", translate("Insert code")]); }
                    if clipboard_available { buttons.extend(["copy", translate("Copy code")]); }
                }
                let hints: HashMap<&str, zbus::zvariant::OwnedValue> = HashMap::new();
                let remaining = (bang_core::otp::MAX_AGE_SECONDS - (chrono::Utc::now().timestamp() - candidate.received_at)).max(1);
                let id: u32 = proxy.call("Notify", &("Bang", 0u32, "io.github.augustin_laurent.Bang", translate("Verification code received"), body, buttons, hints, (remaining * 1000) as i32)).await?;
                live.insert(id, (candidate, target));
            },
            Some(message) = actions.next() => {
                if let Ok((id, action)) = message.body().deserialize::<(u32, String)>() {
                    if !super::otp_enabled() { live.remove(&id); continue; }
                    if action == "copy" {
                        if let Some((candidate, _)) = live.remove(&id) {
                            if bang_core::otp::is_fresh(candidate.received_at, chrono::Utc::now().timestamp()) {
                                let _ = clipboard.send(candidate.code);
                            }
                        }
                    } else if action == "insert" {
                        if let Some((candidate, Some(target))) = live.get(&id).cloned() {
                            if bang_core::otp::is_fresh(candidate.received_at, chrono::Utc::now().timestamp()) && accessibility.insert(&target, &candidate.code).await {
                                live.remove(&id);
                                let _: zbus::Result<()> = proxy.call("CloseNotification", &(id,)).await;
                            } else if clipboard_available && bang_core::otp::is_fresh(candidate.received_at, chrono::Utc::now().timestamp()) {
                                // Notification servers may dismiss the original after
                                // an action, so publish a fresh copy-only fallback.
                                live.remove(&id);
                                let body = notification_body(&candidate);
                                let hints: HashMap<&str, zbus::zvariant::OwnedValue> = HashMap::new();
                                let remaining = (bang_core::otp::MAX_AGE_SECONDS - (chrono::Utc::now().timestamp() - candidate.received_at)).max(1);
                                let fallback: u32 = proxy.call("Notify", &("Bang", 0u32, "io.github.augustin_laurent.Bang", translate("Could not insert; copy the code"), body, vec!["copy", translate("Copy code")], hints, (remaining * 1000) as i32)).await?;
                                live.insert(fallback, (candidate, None));
                            }
                        }
                    }
                }
            },
            Some(message) = closed.next() => {
                if let Ok((id, _)) = message.body().deserialize::<(u32, u32)>() { live.remove(&id); }
            },
            _ = enabled.tick() => {
                if !super::otp_enabled() {
                    let ids: Vec<u32> = live.keys().copied().collect();
                    live.clear();
                    for id in ids {
                        let _: zbus::Result<()> = proxy.call("CloseNotification", &(id,)).await;
                    }
                }
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(next_expiry.unwrap_or(0))), if next_expiry.is_some() => {
                let expired: Vec<u32> = live.iter().filter_map(|(id, (candidate, _))|
                    (!bang_core::otp::is_fresh(candidate.received_at, chrono::Utc::now().timestamp())).then_some(*id)).collect();
                for id in &expired { live.remove(id); }
                for id in expired {
                    let _: zbus::Result<()> = proxy.call("CloseNotification", &(id,)).await;
                }
            },
            else => break,
        }
    }
    Ok(())
}
