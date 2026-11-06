//! Narrow AT-SPI access to the current, explicitly labelled OTP field.

use atspi::{
    events::object::StateChangedEvent,
    proxy::{
        accessible::AccessibleProxy, bus::BusProxy, editable_text::EditableTextProxy,
        registry::RegistryProxy, text::TextProxy,
    },
    Interface, ObjectRefOwned, State,
};
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target(ObjectRefOwned);

#[derive(Clone, Default)]
pub(super) struct Accessibility {
    current: Arc<RwLock<Option<(zbus::Connection, Target)>>>,
}

fn matches_otp_label(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "verification code",
        "code de vérification",
        "security code",
        "code de sécurité",
        "one-time-code",
        "one time code",
        "authentication code",
        "code d’authentification",
        "code d'authentification",
        "otp code",
        "2fa code",
    ]
    .iter()
    .any(|term| text.contains(term))
}

async fn eligible(connection: &zbus::Connection, target: &Target) -> anyhow::Result<bool> {
    let object = &target.0;
    let Some(name) = object.name_as_str() else {
        return Ok(false);
    };
    let proxy = AccessibleProxy::builder(connection)
        .destination(name)?
        .path(object.path_as_str())?
        .build()
        .await?;
    let state = proxy.get_state().await?;
    if !state.contains(State::Focused)
        || !state.contains(State::Editable)
        || state.contains(State::Defunct)
    {
        return Ok(false);
    }
    let role = proxy.get_role_name().await?;
    if !matches!(role.as_str(), "entry" | "text" | "password text") {
        return Ok(false);
    }
    let interfaces = proxy.get_interfaces().await?;
    if !interfaces.contains(Interface::Text) || !interfaces.contains(Interface::EditableText) {
        return Ok(false);
    }
    let name = proxy.name().await.unwrap_or_default();
    let attributes = proxy.get_attributes().await.unwrap_or_default();
    Ok(matches_otp_label(&name)
        || attributes
            .iter()
            .any(|(key, value)| matches_otp_label(key) || matches_otp_label(value)))
}

impl Accessibility {
    pub(super) fn start() -> Self {
        let this = Self::default();
        let shared = this.clone();
        tokio::spawn(async move {
            loop {
                if super::otp_enabled() {
                    let _ = shared.monitor().await;
                    *shared.current.write().await = None;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
        this
    }

    async fn monitor(&self) -> anyhow::Result<()> {
        let session = zbus::Connection::session().await?;
        let address = BusProxy::new(&session).await?.get_address().await?;
        let connection = zbus::connection::Builder::address(address.as_str())?
            .build()
            .await?;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.a11y.atspi.Event.Object")?
            .member("StateChanged")?
            .build();
        let mut signals = zbus::MessageStream::for_match_rule(rule, &connection, Some(128)).await?;
        let registry = RegistryProxy::new(&connection).await?;
        registry
            .register_event("object:state-changed:focused")
            .await?;
        let mut enabled = tokio::time::interval(std::time::Duration::from_secs(1));
        // Browsers can emit Focused while a page is still loading, before the
        // accessible field has its final editable and enabled states.
        let mut probe = tokio::time::interval(std::time::Duration::from_millis(250));
        let mut pending: Option<(Target, std::time::Instant)> = None;
        loop {
            tokio::select! {
                _ = enabled.tick() => if !super::otp_enabled() { break; },
                _ = probe.tick(), if pending.is_some() => {
                    let (target, until) = pending.as_ref().unwrap();
                    if std::time::Instant::now() >= *until {
                        pending = None;
                    } else if tokio::time::timeout(std::time::Duration::from_millis(500), eligible(&connection, target)).await
                        .is_ok_and(|r| r.unwrap_or(false)) {
                        *self.current.write().await = Some((connection.clone(), target.clone()));
                        pending = None;
                    }
                },
                event = signals.next() => {
                    let Some(Ok(message)) = event else { break };
                    let event = match StateChangedEvent::try_from(&message) {
                        Ok(event) => event,
                        Err(_) => continue,
                    };
                    if event.state != State::Focused { continue; }
                    let target = Target(event.item);
                    if event.enabled {
                        let valid = tokio::time::timeout(std::time::Duration::from_millis(500), eligible(&connection, &target)).await
                            .is_ok_and(|r| r.unwrap_or(false));
                        *self.current.write().await = valid.then(|| (connection.clone(), target.clone()));
                        pending = (!valid).then(|| (target, std::time::Instant::now() + std::time::Duration::from_secs(3)));
                    } else {
                        let mut current = self.current.write().await;
                        if current.as_ref().is_some_and(|(_, existing)| *existing == target) { *current = None; }
                        if pending.as_ref().is_some_and(|(existing, _)| *existing == target) { pending = None; }
                    }
                }
            }
        }
        let _ = registry
            .deregister_event("object:state-changed:focused")
            .await;
        Ok(())
    }

    pub(super) async fn target(&self) -> Option<Target> {
        let (connection, target) = self.current.read().await.clone()?;
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            eligible(&connection, &target),
        )
        .await
        .is_ok_and(|result| result.unwrap_or(false))
        .then_some(target)
    }

    pub(super) async fn insert(&self, target: &Target, code: &str) -> bool {
        let Some((connection, current)) = self.current.read().await.clone() else {
            return false;
        };
        if &current != target {
            return false;
        }
        let attempt = async {
            if !eligible(&connection, target).await? {
                return Ok(false);
            }
            let Some(name) = target.0.name_as_str() else {
                return Ok(false);
            };
            let text = TextProxy::builder(&connection)
                .destination(name)?
                .path(target.0.path_as_str())?
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await?;
            if text.character_count().await? != 0 {
                return Ok(false);
            }
            let editable = EditableTextProxy::builder(&connection)
                .destination(name)?
                .path(target.0.path_as_str())?
                .build()
                .await?;
            if !eligible(&connection, target).await? {
                return Ok(false);
            }
            if !editable.set_text_contents(code).await? {
                return Ok(false);
            }
            // Some browser versions report success without updating the field.
            // Use the explicit character count: not every implementation
            // handles the AT-SPI end-of-text sentinel consistently.
            for attempt in 0..6 {
                if attempt != 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                }
                let length = text.character_count().await?;
                if length == code.chars().count() as i32 && text.get_text(0, length).await? == code
                {
                    return Ok::<bool, anyhow::Error>(true);
                }
            }
            Ok::<bool, anyhow::Error>(false)
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), attempt)
            .await
            .is_ok_and(|r| r.unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::matches_otp_label;
    #[test]
    fn requires_an_explicit_verification_label() {
        assert!(matches_otp_label("Enter your verification code"));
        assert!(matches_otp_label("Code de vérification"));
        assert!(!matches_otp_label("Password"));
        assert!(!matches_otp_label("Postal code"));
    }
}
