//! Login sessions from systemd-logind over D-Bus.

use std::collections::HashMap;

use zbus::blocking::{Connection, Proxy};
use zbus::proxy::MethodFlags;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub id: String,
    pub uid: u32,
    pub user: String,
    pub seat: String,
    /// wayland, x11, tty…
    pub kind: String,
    /// active, online, closing
    pub state: String,
    pub remote: bool,
}

pub struct Logind {
    conn: Option<Connection>,
}

impl Default for Logind {
    fn default() -> Self {
        Self::new()
    }
}

const DEST: &str = "org.freedesktop.login1";

impl Logind {
    pub fn new() -> Self {
        Self { conn: Connection::system().ok() }
    }

    fn manager(&self) -> zbus::Result<Proxy<'_>> {
        let conn = self.conn.as_ref().ok_or_else(|| zbus::Error::Failure("D-Bus is not available".into()))?;
        Proxy::new(conn, DEST, "/org/freedesktop/login1", "org.freedesktop.login1.Manager")
    }

    /// User sessions (logind's per-user "manager" sessions are left out).
    pub fn sessions(&self) -> zbus::Result<Vec<Session>> {
        let rows: Vec<(String, u32, String, String, OwnedObjectPath)> = self.manager()?.call("ListSessions", &())?;
        let conn = self.conn.as_ref().expect("checked by manager()");
        let mut out = Vec::new();
        for (id, uid, user, seat, path) in rows {
            let reply = conn.call_method(
                Some(DEST),
                path.as_str(),
                Some("org.freedesktop.DBus.Properties"),
                "GetAll",
                &("org.freedesktop.login1.Session",),
            )?;
            let props: HashMap<String, OwnedValue> = reply.body().deserialize()?;
            let text = |k: &str| props.get(k).and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
            if text("Class") != "user" {
                continue;
            }
            out.push(Session {
                id,
                uid,
                user,
                seat,
                kind: text("Type"),
                state: text("State"),
                remote: props.get("Remote").and_then(|v| bool::try_from(v.clone()).ok()).unwrap_or(false),
            });
        }
        Ok(out)
    }

    /// "lock" or "terminate" (sign out). Other users' sessions need polkit authorization.
    pub fn control(&self, verb: &str, session: &str) -> zbus::Result<()> {
        let method = match verb {
            "lock" => "LockSession",
            "terminate" => "TerminateSession",
            _ => return Err(zbus::Error::Failure(format!("unknown action {verb}"))),
        };
        self.manager()?.call_with_flags::<_, _, ()>(method, MethodFlags::AllowInteractiveAuth.into(), &(session,))?;
        Ok(())
    }
}
