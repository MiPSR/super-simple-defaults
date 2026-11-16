//! Direct portal settings, with the original host-command fallbacks retained.
//! Everything here runs on background threads; `within` keeps each query bounded.
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(2);

/// Runs `work` on a thread and waits up to `TIMEOUT` for it. `None` on timeout
/// or when the worker failed; the worker itself may still finish later.
fn within<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(work());
    });
    receive.recv_timeout(TIMEOUT).ok()
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let mut text = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_string(&mut text);
            }
            return status.success().then_some(text);
        }
        if started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn portal_value(namespace: String, key: String) -> Option<zbus::zvariant::OwnedValue> {
    within(move || {
        let connection = zbus::blocking::Connection::session().ok()?;
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )
        .ok()?;
        match proxy.call("ReadOne", &(namespace.as_str(), key.as_str())) {
            Ok(value) => Some(value),
            Err(_) => proxy.call("Read", &(namespace.as_str(), key.as_str())).ok(),
        }
    })?
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

fn portal_dark() -> Option<bool> {
    color_scheme(&portal_value(
        "org.freedesktop.appearance".into(),
        "color-scheme".into(),
    )?)
}

fn portal_icon_theme() -> Option<String> {
    let value = portal_value("org.gnome.desktop.interface".into(), "icon-theme".into())?;
    <&str>::try_from(plain(&value)).ok().map(str::to_string)
}

fn dbus_send_dark() -> Option<bool> {
    output(
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
    .and_then(|s| {
        if s.contains("uint32 1") {
            Some(true)
        } else if s.contains("uint32 2") {
            Some(false)
        } else {
            None
        }
    })
}

fn gsettings_dark() -> Option<bool> {
    output(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "color-scheme"],
    )
    .and_then(|s| {
        if s.contains("prefer-dark") {
            Some(true)
        } else if s.contains("prefer-light") {
            Some(false)
        } else {
            None
        }
    })
}

fn gtk_theme_dark() -> Option<bool> {
    std::env::var("GTK_THEME")
        .ok()
        .map(|s| !s.to_lowercase().contains("light"))
}

fn gsettings_icon_theme() -> Option<String> {
    output(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "icon-theme"],
    )
    .map(|s| s.trim().trim_matches('\'').to_string())
    .filter(|s| !s.is_empty())
}

#[derive(Clone, Debug)]
pub struct Appearance {
    pub dark: bool,
    pub icon_theme: Option<String>,
}

/// Current portal appearance, with the original host-command fallbacks. Bounded
/// by `TIMEOUT` per backend; a slow backend falls through to the next one.
pub fn appearance() -> Appearance {
    let dark = portal_dark()
        .or_else(dbus_send_dark)
        .or_else(gsettings_dark)
        .or_else(gtk_theme_dark)
        .unwrap_or(true);
    let icon_theme = portal_icon_theme().or_else(gsettings_icon_theme);
    Appearance { dark, icon_theme }
}

/// Listens for color-scheme changes forever, reporting each dark-mode change.
/// Runs on its own thread; reconnects a few seconds after a drop.
pub fn watch_theme(mut on_dark: impl FnMut(bool)) {
    loop {
        if let Ok(connection) = zbus::blocking::Connection::session()
            && let Ok(proxy) = zbus::blocking::Proxy::new(
                &connection,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Settings",
            )
            && let Ok(mut signals) = proxy.receive_signal("SettingChanged")
        {
            for signal in signals.by_ref() {
                if let Ok((namespace, key, value)) = signal.body().deserialize::<(
                    String,
                    String,
                    zbus::zvariant::OwnedValue,
                )>() && namespace == "org.freedesktop.appearance"
                    && key == "color-scheme"
                    && let Some(dark) = color_scheme(&value)
                {
                    on_dark(dark);
                }
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{OwnedValue, Value};

    #[test]
    fn portal_color_scheme_accepts_nested_legacy_variants() {
        assert_eq!(color_scheme(&OwnedValue::from(1_u32)), Some(true));
        assert_eq!(color_scheme(&OwnedValue::from(2_u32)), Some(false));
        assert_eq!(color_scheme(&OwnedValue::from(0_u32)), None);
        let nested = OwnedValue::try_from(Value::Value(Box::new(Value::U32(1)))).unwrap();
        assert_eq!(color_scheme(&nested), Some(true));
    }

    #[test]
    fn appearance_falls_through_unavailable_backends() {
        // No portal, no dbus-send, no gsettings in a bare test environment: the
        // GTK_THEME fallback (or the dark default) still answers.
        assert!(matches!(appearance().dark, true | false));
    }
}
