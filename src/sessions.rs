use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub password: String,
    /// SSH username for the SFTP file-transfer panel (optional; remembered per host).
    #[serde(default)]
    pub ssh_user: String,
}

impl Session {
    pub fn display_label(&self) -> String {
        if self.name.is_empty() {
            format!("{}:{}", self.host, self.port)
        } else {
            format!("{} ({}:{})", self.name, self.host, self.port)
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    sessions: Vec<Session>,
}

/// Primary location: `sessions.json` right next to the executable. This is a
/// fixed, predictable path that resolves identically regardless of how the app
/// is launched or which user context resolves it — unlike `config_dir()`,
/// which can differ between interactive and non-interactive sessions on
/// domain/redirected profiles.
fn exe_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("sessions.json"))
}

/// Legacy location under the user's config dir (kept only for one-way migration
/// so previously-saved sessions aren't lost).
fn config_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ironvnc");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("sessions.json")
}

/// The path the app reads from and writes to (shown in the UI).
pub fn path() -> PathBuf {
    exe_path().unwrap_or_else(config_path)
}

pub fn display_path() -> String {
    path().display().to_string()
}

pub fn load() -> Vec<Session> {
    // Primary (next-to-exe) is authoritative when present.
    if let Some(p) = exe_path() {
        if let Ok(content) = std::fs::read_to_string(&p) {
            if let Ok(store) = serde_json::from_str::<Store>(&content) {
                return store.sessions;
            }
        }
    }
    // Migration fallback: read the old config-dir file if the primary is absent.
    if let Ok(content) = std::fs::read_to_string(config_path()) {
        if let Ok(store) = serde_json::from_str::<Store>(&content) {
            return store.sessions;
        }
    }
    Vec::new()
}

pub fn save(sessions: &[Session]) -> Result<()> {
    let store = Store {
        sessions: sessions.to_vec(),
    };
    let json = serde_json::to_string_pretty(&store)?;
    // Prefer next-to-exe; fall back to config dir only if that write fails
    // (e.g. the exe lives in a read-only directory).
    if let Some(p) = exe_path() {
        if std::fs::write(&p, &json).is_ok() {
            return Ok(());
        }
    }
    std::fs::write(config_path(), json)?;
    Ok(())
}
