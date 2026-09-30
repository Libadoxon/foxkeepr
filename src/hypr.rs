//! Hyprland IPC. Uses post 0.56 Lua syntax.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tracing::trace;

use crate::event::normalize_addr;

#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Client {
    pub address: String,
    pub class: String,
    pub title: String,
    pub workspace: Workspace,
}

pub struct Hypr {
    dir: PathBuf,
}

impl Hypr {
    pub fn from_env() -> io::Result<Self> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "HYPRLAND_INSTANCE_SIGNATURE is unset - is Hyprland running?",
            )
        })?;
        let runtime = std::env::var("XDG_RUNTIME_DIR")
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is unset"))?;
        let dir = PathBuf::from(runtime).join("hypr").join(sig);
        if !dir.join(".socket.sock").exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no Hyprland control socket at {}", dir.display()),
            ));
        }
        Ok(Self { dir })
    }

    fn request(&self, payload: &str) -> io::Result<String> {
        trace!(%payload, "ipc request");
        let mut sock = UnixStream::connect(self.dir.join(".socket.sock"))?;
        sock.set_read_timeout(Some(Duration::from_secs(5)))?;
        sock.write_all(payload.as_bytes())?;
        sock.shutdown(Shutdown::Write)?;
        let mut out = String::new();
        sock.read_to_string(&mut out)?;
        Ok(out)
    }

    pub fn clients(&self) -> io::Result<Vec<Client>> {
        let json = self.request("j/clients")?;
        serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Move a window to a workspace *without* pulling the user's view along
    pub fn move_window(&self, addr: &str, workspace: &str) -> io::Result<()> {
        let payload = format!(
            "/dispatch hl.dsp.window.move({{ workspace = {}, window = {}, follow = false }})",
            workspace_lua(workspace),
            lua_string(&format!("address:{}", normalize_addr(addr))),
        );
        let reply = self.request(&payload)?;
        if reply.trim() == "ok" {
            Ok(())
        } else {
            Err(io::Error::other(reply.trim().to_string()))
        }
    }

    pub fn events(&self, poll: Duration) -> io::Result<Events> {
        let sock = UnixStream::connect(self.dir.join(".socket2.sock"))?;
        sock.set_read_timeout(Some(poll))?;
        Ok(Events {
            sock,
            buf: Vec::new(),
        })
    }
}

/// Line reader over the event socket. `next_line` returns `Ok(None)` when the
/// poll timeout elapses, which is how the caller gets a chance to run timers.
pub struct Events {
    sock: UnixStream,
    buf: Vec<u8>,
}

impl Events {
    pub fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            if let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
                let rest = self.buf.split_off(nl + 1);
                let line = std::mem::replace(&mut self.buf, rest);
                return Ok(Some(
                    String::from_utf8_lossy(&line[..nl]).trim_end().to_string(),
                ));
            }
            let mut chunk = [0u8; 4096];
            match self.sock.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Hyprland closed the event socket",
                    ));
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

fn lua_string(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Render a workspace name as the Lua value the move dispatcher expects.
pub fn workspace_lua(name: &str) -> String {
    if name.parse::<i32>().is_ok() {
        name.to_string()
    } else if name == "special" || name.starts_with("special:") {
        lua_string(name)
    } else {
        lua_string(&format!("name:{name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_workspaces_pass_through_as_integers() {
        assert_eq!(workspace_lua("4"), "4");
        assert_eq!(workspace_lua("-98"), "-98");
    }

    #[test]
    fn special_workspaces_keep_their_prefix() {
        assert_eq!(workspace_lua("special:mail"), "'special:mail'");
    }

    #[test]
    fn named_workspaces_get_the_name_selector() {
        assert_eq!(workspace_lua("code"), "'name:code'");
    }

    #[test]
    fn quotes_in_names_are_escaped() {
        assert_eq!(workspace_lua("it's"), "'name:it\\'s'");
    }
}
