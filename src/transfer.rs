//! SFTP side-channel file transfer — a MobaXterm-style remote file browser that
//! rides alongside the VNC session over SSH (port 22).
//!
//! The VNC protocol has no standard file transfer, and these servers span two
//! proprietary VNC ecosystems, so we move files out-of-band over SFTP instead.
//! SSH credentials are the target's OS login, which differ from the VNC
//! password, so the UI prompts for them separately.
//!
//! Like the VNC connection, this runs on a dedicated thread with its own
//! single-threaded tokio runtime and talks to the UI over crossbeam channels.

use crossbeam_channel::{Receiver, Sender};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One entry in a remote directory listing.
#[derive(Clone, Debug)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// UI → background commands.
pub enum TransferCommand {
    ListDir(String),
    Upload { local: PathBuf, remote_dir: String },
    Download { remote: String, local_dir: PathBuf },
    Mkdir(String),
    Disconnect,
}

/// Background → UI events.
pub enum TransferEvent {
    Connected { home: String },
    DirListing { path: String, entries: Vec<RemoteEntry> },
    Status(String),
    TransferDone { name: String },
    Error(String),
    Disconnected,
}

pub struct TransferParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

pub struct TransferSession {
    pub event_rx: Receiver<TransferEvent>,
    pub cmd_tx: Sender<TransferCommand>,
}

impl TransferSession {
    pub fn connect(params: TransferParams) -> Self {
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<TransferEvent>();
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<TransferCommand>();

        thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = event_tx.send(TransferEvent::Error(format!("runtime: {e}")));
                    return;
                }
            };
            rt.block_on(async move {
                if let Err(e) = run(params, &event_tx, cmd_rx).await {
                    let _ = event_tx.send(TransferEvent::Error(e.to_string()));
                }
                let _ = event_tx.send(TransferEvent::Disconnected);
            });
        });

        Self { event_rx, cmd_tx }
    }

    pub fn list_dir(&self, path: String) {
        let _ = self.cmd_tx.send(TransferCommand::ListDir(path));
    }
    pub fn upload(&self, local: PathBuf, remote_dir: String) {
        let _ = self.cmd_tx.send(TransferCommand::Upload { local, remote_dir });
    }
    pub fn download(&self, remote: String, local_dir: PathBuf) {
        let _ = self
            .cmd_tx
            .send(TransferCommand::Download { remote, local_dir });
    }
    pub fn mkdir(&self, path: String) {
        let _ = self.cmd_tx.send(TransferCommand::Mkdir(path));
    }
    pub fn disconnect(&self) {
        let _ = self.cmd_tx.send(TransferCommand::Disconnect);
    }
}

/// Accept any host key. This is an internal-tooling convenience; a hardened
/// build would pin/verify the key.
struct SshHandler;

impl russh::client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

async fn run(
    params: TransferParams,
    event_tx: &Sender<TransferEvent>,
    cmd_rx: Receiver<TransferCommand>,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use russh_sftp::client::SftpSession;

    let _ = event_tx.send(TransferEvent::Status(format!(
        "Connecting to {}:{} ...",
        params.host, params.port
    )));

    let config = Arc::new(russh::client::Config::default());
    let mut handle = russh::client::connect(config, (params.host.as_str(), params.port), SshHandler)
        .await
        .with_context(|| format!("SSH connect to {}:{}", params.host, params.port))?;

    let auth = handle
        .authenticate_password(&params.username, &params.password)
        .await
        .context("SSH password authentication")?;
    if !auth.success() {
        anyhow::bail!("SSH authentication failed (wrong username or password?)");
    }

    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let sftp = SftpSession::new(channel.into_stream()).await?;

    // Resolve the login directory to an absolute path for a sensible start.
    let home = sftp
        .canonicalize(".")
        .await
        .unwrap_or_else(|_| ".".to_string());
    let _ = event_tx.send(TransferEvent::Connected { home: home.clone() });
    send_listing(&sftp, &home, event_tx).await;

    // Command loop. crossbeam recv is blocking; poll it without starving the
    // runtime by yielding between checks.
    loop {
        let cmd = match cmd_rx.try_recv() {
            Ok(c) => c,
            Err(crossbeam_channel::TryRecvError::Empty) => {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                continue;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => break,
        };

        match cmd {
            TransferCommand::Disconnect => break,
            TransferCommand::ListDir(path) => send_listing(&sftp, &path, event_tx).await,
            TransferCommand::Mkdir(path) => {
                match sftp.create_dir(&path).await {
                    Ok(_) => {
                        let _ = event_tx.send(TransferEvent::Status(format!("Created {path}")));
                        if let Some(parent) = parent_of(&path) {
                            send_listing(&sftp, &parent, event_tx).await;
                        }
                    }
                    Err(e) => {
                        let _ = event_tx.send(TransferEvent::Error(format!("mkdir {path}: {e}")));
                    }
                }
            }
            TransferCommand::Upload { local, remote_dir } => {
                if let Err(e) = upload(&sftp, &local, &remote_dir, event_tx).await {
                    let _ = event_tx.send(TransferEvent::Error(format!(
                        "upload {}: {e}",
                        local.display()
                    )));
                } else {
                    send_listing(&sftp, &remote_dir, event_tx).await;
                }
            }
            TransferCommand::Download { remote, local_dir } => {
                if let Err(e) = download(&sftp, &remote, &local_dir, event_tx).await {
                    let _ = event_tx.send(TransferEvent::Error(format!("download {remote}: {e}")));
                }
            }
        }
    }

    Ok(())
}

async fn send_listing(
    sftp: &russh_sftp::client::SftpSession,
    path: &str,
    event_tx: &Sender<TransferEvent>,
) {
    match sftp.read_dir(path).await {
        Ok(dir) => {
            let mut entries: Vec<RemoteEntry> = dir
                .map(|e| RemoteEntry {
                    name: e.file_name(),
                    is_dir: e.metadata().is_dir(),
                    size: e.metadata().size.unwrap_or(0),
                })
                .filter(|e| e.name != "." && e.name != "..")
                .collect();
            // Directories first, then case-insensitive by name.
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            let _ = event_tx.send(TransferEvent::DirListing {
                path: path.to_string(),
                entries,
            });
        }
        Err(e) => {
            let _ = event_tx.send(TransferEvent::Error(format!("list {path}: {e}")));
        }
    }
}

async fn upload(
    sftp: &russh_sftp::client::SftpSession,
    local: &std::path::Path,
    remote_dir: &str,
    event_tx: &Sender<TransferEvent>,
) -> anyhow::Result<()> {
    let name = local
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("invalid local file name"))?;
    let remote_path = join_remote(remote_dir, &name);

    let _ = event_tx.send(TransferEvent::Status(format!("Uploading {name} ...")));
    let data = tokio::fs::read(local).await?;
    let mut f = sftp.create(&remote_path).await?;
    f.write_all(&data).await?;
    f.shutdown().await?;
    let _ = event_tx.send(TransferEvent::TransferDone { name });
    Ok(())
}

async fn download(
    sftp: &russh_sftp::client::SftpSession,
    remote: &str,
    local_dir: &std::path::Path,
    event_tx: &Sender<TransferEvent>,
) -> anyhow::Result<()> {
    let name = remote.rsplit('/').next().unwrap_or(remote).to_string();
    let local_path = local_dir.join(&name);

    let _ = event_tx.send(TransferEvent::Status(format!("Downloading {name} ...")));
    let mut f = sftp.open(remote).await?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await?;
    tokio::fs::write(&local_path, &buf).await?;
    let _ = event_tx.send(TransferEvent::TransferDone { name });
    Ok(())
}

/// Join a remote dir and a file name with a single '/'.
fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Parent path of a remote path (POSIX-style), or None for a top-level entry.
fn parent_of(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(i) => Some(trimmed[..i].to_string()),
        None => None,
    }
}
