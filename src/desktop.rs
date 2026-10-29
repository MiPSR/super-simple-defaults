//! Direct portal settings, with the original host-command fallbacks retained.
use std::time::Duration;
use tokio::process::Command;

async fn output(program: &str, args: &[&str]) -> Option<String> {
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).into_owned())
}

async fn portal_value(namespace: &str, key: &str) -> Option<zbus::zvariant::OwnedValue> {
    let connection = zbus::Connection::session().await.ok()?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )
    .await
    .ok()?;
    match proxy.call("ReadOne", &(namespace, key)).await {
        Ok(value) => Some(value),
        Err(_) => proxy.call("Read", &(namespace, key)).await.ok(),
    }
}

fn plain(value: &zbus::zvariant::OwnedValue) -> &zbus::zvariant::Value<'static> {
    let mut value: &zbus::zvariant::Value<'static> = value;
    while let zbus::zvariant::Value::Value(inner) = value {
        value = inner;
    }
    value
}

fn color_scheme(value: &zbus::zvariant::OwnedValue) -> Option<bool> {
    match u32::try_from(plain(value)).ok()? {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

async fn portal_dark() -> Option<bool> {
    color_scheme(&portal_value("org.freedesktop.appearance", "color-scheme").await?)
}

#[derive(Clone, Debug)]
pub struct Appearance {
    pub dark: bool,
    pub icon_theme: Option<String>,
}

pub async fn appearance() -> Appearance {
    let dark = if let Some(dark) = tokio::time::timeout(Duration::from_secs(2), portal_dark())
        .await
        .ok()
        .flatten()
    {
        dark
    } else if let Some(dark) = output(
        "dbus-send",
        &[
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings.Read",
            "string:org.freedesktop.appearance",
            "string:color-scheme",
        ],
    )
    .await
    .and_then(|s| {
        if s.contains("uint32 1") {
            Some(true)
        } else if s.contains("uint32 2") {
            Some(false)
        } else {
            None
        }
    }) {
        dark
    } else if let Some(dark) = output(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "color-scheme"],
    )
    .await
    .and_then(|s| {
        if s.contains("prefer-dark") {
            Some(true)
        } else if s.contains("prefer-light") {
            Some(false)
        } else {
            None
        }
    }) {
        dark
    } else {
        std::env::var("GTK_THEME")
            .map(|s| !s.to_lowercase().contains("light"))
            .unwrap_or(true)
    };
    let icon_theme = match tokio::time::timeout(
        Duration::from_secs(2),
        portal_value("org.gnome.desktop.interface", "icon-theme"),
    )
    .await
    .ok()
    .flatten()
    .and_then(|v| <&str>::try_from(plain(&v)).ok().map(str::to_string))
    {
        Some(theme) => Some(theme),
        None => output(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "icon-theme"],
        )
        .await
        .map(|s| s.trim().trim_matches('\'').to_string())
        .filter(|s| !s.is_empty()),
    };
    Appearance { dark, icon_theme }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirmation {
    Yes,
    No,
    Unavailable,
}

fn decision(status: std::process::ExitStatus) -> Option<Confirmation> {
    if status.success() {
        Some(Confirmation::Yes)
    } else if status.code() == Some(1) {
        Some(Confirmation::No)
    } else {
        None
    }
}

pub async fn confirm_reset() -> Confirmation {
    // Only missing/broken backends fall through; a user's No never does.
    for (program, args) in [
        (
            "kdialog",
            vec![
                "--yesno",
                "Reset all user default applications? Custom associations will be preserved.",
                "--title",
                "Reset defaults",
            ],
        ),
        (
            "zenity",
            vec![
                "--question",
                "--title=Reset defaults",
                "--text=Reset all user default applications? Custom associations will be preserved.",
            ],
        ),
    ] {
        if let Ok(status) = Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .status()
            .await
            && let Some(answer) = decision(status)
        {
            return answer;
        }
    }
    Confirmation::Unavailable
}

pub fn theme_changes() -> impl iced::futures::Stream<Item = bool> {
    iced::stream::channel(8, |mut output| async move {
        use iced::futures::{SinkExt, StreamExt};
        loop {
            if let Ok(connection) = zbus::Connection::session().await
                && let Ok(proxy) = zbus::Proxy::new(
                    &connection,
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.Settings",
                )
                .await
                && let Ok(mut signals) = proxy.receive_signal("SettingChanged").await
            {
                while let Some(signal) = signals.next().await {
                    if let Ok((namespace, key, value)) =
                        signal
                            .body()
                            .deserialize::<(String, String, zbus::zvariant::OwnedValue)>()
                        && namespace == "org.freedesktop.appearance"
                        && key == "color-scheme"
                        && let Some(dark) = color_scheme(&value)
                        && output.send(dark).await.is_err()
                    {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn native_dialog_cancellation_is_final_not_a_backend_failure() {
        assert_eq!(
            decision(std::process::ExitStatus::from_raw(0)),
            Some(Confirmation::Yes)
        );
        assert_eq!(
            decision(std::process::ExitStatus::from_raw(256)),
            Some(Confirmation::No)
        );
        assert_eq!(decision(std::process::ExitStatus::from_raw(512)), None);
    }

    #[test]
    fn portal_color_scheme_accepts_nested_legacy_variants() {
        use zbus::zvariant::{OwnedValue, Value};
        assert_eq!(color_scheme(&OwnedValue::from(1_u32)), Some(true));
        assert_eq!(color_scheme(&OwnedValue::from(2_u32)), Some(false));
        assert_eq!(color_scheme(&OwnedValue::from(0_u32)), None);
        let nested = OwnedValue::try_from(Value::Value(Box::new(Value::U32(1)))).unwrap();
        assert_eq!(color_scheme(&nested), Some(true));
    }
}
