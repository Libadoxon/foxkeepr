//! The persisted layout: one record per browser window that was open the last
//! time we saw a healthy session.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Creation order. Also the tie-breaker when matching by position.
    pub seq: u64,
    pub class: String,
    pub title: String,
    /// Workspace *name* (`"4"`, `"special:mail"`, `"code"`)
    pub workspace: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub next_seq: u64,
    #[serde(default)]
    pub session: Vec<Record>,
}

impl State {
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Write via a temp file + rename so a crash mid-write cannot truncate the
    /// layout we are trying to protect.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        text.push('\n');
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("foxkeepr/session.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_loads_as_empty() {
        let path = Path::new("/nonexistent-foxkeepr/session.json");
        let state = State::load(path).unwrap();
        assert!(state.session.is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("foxkeepr-test-{}", std::process::id()));
        let path = dir.join("session.json");
        let state = State {
            next_seq: 3,
            session: vec![Record {
                seq: 1,
                class: "floorp".into(),
                title: "Docs — Floorp".into(),
                workspace: "special:mail".into(),
            }],
        };
        state.save(&path).unwrap();
        let back = State::load(&path).unwrap();
        assert_eq!(back.next_seq, 3);
        assert_eq!(back.session, state.session);
        fs::remove_dir_all(&dir).ok();
    }
}
