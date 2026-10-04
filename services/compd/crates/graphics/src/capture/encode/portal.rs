//! XDG Desktop Portal `FileChooser.SaveFile` — the "Save As" dialog.
//!
//! Blocking; intended to run on a background thread (it waits for the user to
//! pick a file). Returns `None` on any failure (no portal, cancelled, parse
//! error) — the caller treats `None` as a no-op and keeps its Save UI open.

use std::collections::HashMap;
use std::path::PathBuf;

/// Open the portal Save dialog. Returns the chosen path, or `None`.
pub fn save_file_dialog(title: &str, suggested_name: &str) -> Option<PathBuf> {
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::Value;

    let conn = Connection::session()
        .map_err(|e| warn!("portal: no session bus ({e}); Save As unavailable"))
        .ok()?;

    let chooser = Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.FileChooser",
    )
    .ok()?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("current_name", Value::from(suggested_name));

    let request: zbus::zvariant::OwnedObjectPath = chooser
        .call("SaveFile", &("", title, options))
        .map_err(|e| warn!("portal: SaveFile call failed ({e})"))
        .ok()?;

    // Wait for the Response signal on the returned request object.
    let req_proxy = Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        request.as_str().to_owned(),
        "org.freedesktop.portal.Request",
    )
    .ok()?;

    let mut signals = req_proxy.receive_signal("Response").ok()?;
    let msg = signals.next()?; // blocks until the user responds
    let (response, results): (u32, HashMap<String, zbus::zvariant::OwnedValue>) =
        msg.body().deserialize().ok()?;
    if response != 0 {
        return None; // cancelled / dismissed
    }

    let uris_val = results.get("uris")?;
    let uris = Vec::<String>::try_from(uris_val.try_clone().ok()?).ok()?;
    let uri = uris.into_iter().next()?;
    Some(uri_to_path(&uri))
}

fn uri_to_path(uri: &str) -> PathBuf {
    let p = uri.strip_prefix("file://").unwrap_or(uri);
    PathBuf::from(percent_decode(p))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
