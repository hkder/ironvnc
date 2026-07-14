use crate::protocol::client::VncClient;
use crossbeam_channel::{Receiver, Sender};
use std::thread;

#[derive(Clone, Debug)]
pub struct ConnectionParams {
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
}

/// A single changed rectangle: tightly-packed `w*h*4` RGBA bytes at (x, y).
pub struct FbRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Messages sent from the VNC background thread to the UI thread.
pub enum VncEvent {
    DesktopSize(u32, u32),
    /// Only the rectangles that changed this update — applied as partial
    /// texture updates so we never re-upload the whole framebuffer.
    FramebufferRects(Vec<FbRect>),
    DesktopName(String),
    /// Server sent clipboard text.
    ClipboardText(String),
    /// Server requires VNC authentication — UI should prompt for password.
    NeedPassword,
    Error(String),
    Disconnected,
}

/// Commands sent from the UI thread to the VNC background thread.
pub enum VncCommand {
    KeyEvent { down: bool, keysym: u32 },
    PointerEvent { buttons: u8, x: u16, y: u16 },
    /// Send clipboard text to the server.
    SetClipboard(String),
    /// Response to NeedPassword: provide the password to the waiting auth step.
    ProvidePassword(String),
    Disconnect,
}

pub struct VncConnection {
    pub event_rx: Receiver<VncEvent>,
    pub cmd_tx: Sender<VncCommand>,
}

impl VncConnection {
    pub fn connect(params: ConnectionParams) -> Self {
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<VncEvent>();
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<VncCommand>();

        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");

            rt.block_on(async {
                match VncClient::connect(&params, event_tx.clone(), cmd_rx).await {
                    Ok(()) => {
                        let _ = event_tx.send(VncEvent::Disconnected);
                    }
                    Err(e) => {
                        let _ = event_tx.send(VncEvent::Error(e.to_string()));
                    }
                }
            });
        });

        Self { event_rx, cmd_tx }
    }

    pub fn send_key(&self, down: bool, keysym: u32) {
        let _ = self.cmd_tx.send(VncCommand::KeyEvent { down, keysym });
    }

    pub fn send_pointer(&self, buttons: u8, x: u16, y: u16) {
        let _ = self.cmd_tx.send(VncCommand::PointerEvent { buttons, x, y });
    }

    pub fn send_clipboard(&self, text: String) {
        let _ = self.cmd_tx.send(VncCommand::SetClipboard(text));
    }

    pub fn provide_password(&self, pw: String) {
        let _ = self.cmd_tx.send(VncCommand::ProvidePassword(pw));
    }

    pub fn disconnect(&self) {
        let _ = self.cmd_tx.send(VncCommand::Disconnect);
    }
}
