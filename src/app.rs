use crate::connection::{ConnectionParams, VncConnection, VncEvent};
use crate::sessions::{self, Session};
use crate::transfer::{RemoteEntry, TransferEvent, TransferParams, TransferSession};
use egui::{Color32, Key, Modifiers, Rect, Sense, TextureHandle, TextureOptions, Vec2};

pub struct VncApp {
    // Toolbar address field ("host" or "host:port")
    address_input: String,

    // Active VNC session
    connection: Option<VncConnection>,
    texture: Option<TextureHandle>,
    fb_width: u32,
    fb_height: u32,
    desktop_name: String,
    status_msg: String,

    // Password dialog — only shown when server explicitly requests auth
    show_password_dialog: bool,
    pending_password: String,

    // Sessions popup window
    show_sessions: bool,
    sessions: Vec<Session>,
    session_selected: Option<usize>,
    sf_name: String,
    sf_host: String,
    sf_port: String,
    sf_password: String,
    sf_editing: Option<usize>,

    // Input state
    last_modifiers: Modifiers,
    last_mouse_pos: (u16, u16),
    clipboard: Option<arboard::Clipboard>,

    // One-time window sizing
    first_frame: bool,

    // Active VNC connection details (for the SFTP side-channel + auto-save)
    current_host: String,
    current_port: u16,
    current_password: String,

    // SFTP file-transfer panel (MobaXterm-style)
    transfer: Option<TransferSession>,
    show_transfer: bool,
    ssh_user: String,
    ssh_pass: String,
    transfer_status: String,
    remote_path: String,
    remote_entries: Vec<RemoteEntry>,
    remote_selected: Option<usize>,
}

impl VncApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial: Option<ConnectionParams>) -> Self {
        let sessions = sessions::load();
        let clipboard = arboard::Clipboard::new().ok();

        let mut app = Self {
            address_input: String::new(),
            connection: None,
            texture: None,
            fb_width: 0,
            fb_height: 0,
            desktop_name: String::new(),
            status_msg: "Not connected".to_string(),
            show_password_dialog: false,
            pending_password: String::new(),
            show_sessions: false,
            sessions,
            session_selected: None,
            sf_name: String::new(),
            sf_host: String::new(),
            sf_port: "5900".to_string(),
            sf_password: String::new(),
            sf_editing: None,
            last_modifiers: Modifiers::default(),
            last_mouse_pos: (0, 0),
            clipboard,
            first_frame: true,
            current_host: String::new(),
            current_port: 5900,
            current_password: String::new(),
            transfer: None,
            show_transfer: false,
            ssh_user: String::new(),
            ssh_pass: String::new(),
            transfer_status: String::new(),
            remote_path: String::new(),
            remote_entries: Vec::new(),
            remote_selected: None,
        };

        if let Some(params) = initial {
            app.address_input = if params.port == 5900 {
                params.host.clone()
            } else {
                format!("{}:{}", params.host, params.port)
            };
            app.do_connect(params.host, params.port, params.password);
        }

        app
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn parse_address(input: &str) -> (String, u16) {
        let s = input.trim();
        if let Some(pos) = s.rfind(':') {
            let port_part = &s[pos + 1..];
            if let Ok(port) = port_part.parse::<u16>() {
                return (s[..pos].to_string(), port);
            }
        }
        (s.to_string(), 5900)
    }

    /// Start a connection immediately — no pre-connect dialog.
    /// The server will tell us if it needs auth (via VncEvent::NeedPassword).
    fn do_connect(&mut self, host: String, port: u16, password: Option<String>) {
        self.status_msg = format!("Connecting to {}:{}…", host, port);
        self.desktop_name = String::new();
        self.show_password_dialog = false;
        self.pending_password.clear();
        self.current_host = host.clone();
        self.current_port = port;
        self.current_password = password.clone().unwrap_or_default();
        // Remember the SSH username saved for this host, for the SFTP panel.
        self.ssh_user = self
            .sessions
            .iter()
            .find(|s| s.host == host && !s.ssh_user.is_empty())
            .map(|s| s.ssh_user.clone())
            .unwrap_or_default();
        self.connection = Some(VncConnection::connect(ConnectionParams {
            host,
            port,
            password,
        }));
    }

    /// Open a saved session in its own OS window by launching a new process.
    /// Passing the session key (not the password) keeps credentials off the
    /// command line — the child loads the password from the saved sessions file.
    /// Launch a new instance (window) of this app with the given args.
    fn spawn_window(args: &[&str]) {
        if let Ok(exe) = std::env::current_exe() {
            let mut cmd = std::process::Command::new(exe);
            cmd.args(args);
            // Don't spawn an extra console window for the child on Windows.
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            let _ = cmd.spawn();
        }
    }

    /// Open a saved session in its own window. Passing the session key (not the
    /// password) keeps credentials off the command line — the child loads the
    /// password from the saved sessions file.
    fn open_session_window(session: &Session) {
        let key = if session.name.is_empty() {
            &session.host
        } else {
            &session.name
        };
        Self::spawn_window(&["--session", key]);
    }

    /// Open an ad-hoc host:port connection in its own window. The child loads a
    /// saved password for that host:port if one exists, else prompts.
    fn open_host_window(host: &str, port: u16) {
        Self::spawn_window(&["--host", host, "--port", &port.to_string()]);
    }

    /// Tear down the SFTP side-channel, if any.
    fn close_transfer(&mut self) {
        if let Some(t) = &self.transfer {
            t.disconnect();
        }
        self.transfer = None;
        self.show_transfer = false;
        self.remote_entries.clear();
        self.remote_selected = None;
        self.remote_path.clear();
        self.transfer_status.clear();
    }

    fn disconnect(&mut self, ctx: &egui::Context) {
        if let Some(conn) = &self.connection {
            conn.disconnect();
        }
        self.connection = None;
        self.texture = None;
        self.show_password_dialog = false;
        self.status_msg = "Disconnected".to_string();
        self.desktop_name = String::new();
        self.close_transfer();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title("IronVNC".to_string()));
    }

    fn update_status(&mut self) {
        self.status_msg = if self.desktop_name.is_empty() {
            format!("Connected  {}×{}", self.fb_width, self.fb_height)
        } else {
            format!("{}  {}×{}", self.desktop_name, self.fb_width, self.fb_height)
        };
    }

    // ── Event processing ──────────────────────────────────────────────────────

    fn process_events(&mut self, ctx: &egui::Context) {
        let events: Vec<VncEvent> = match &self.connection {
            Some(conn) => conn.event_rx.try_iter().collect(),
            None => return,
        };

        for event in events {
            match event {
                VncEvent::DesktopSize(w, h) => {
                    let resized = self.fb_width != w || self.fb_height != h;
                    self.fb_width = w;
                    self.fb_height = h;
                    // (Re)allocate the full texture as black; partial updates
                    // paint into it as rectangles arrive.
                    if resized || self.texture.is_none() {
                        let black = egui::ColorImage::new(
                            [w as usize, h as usize],
                            Color32::BLACK,
                        );
                        self.texture = Some(ctx.load_texture(
                            "vnc-fb",
                            black,
                            TextureOptions::LINEAR,
                        ));
                    }
                    self.update_status();
                }
                VncEvent::DesktopName(name) => {
                    self.desktop_name = name.clone();
                    self.update_status();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                        "{} — IronVNC",
                        name
                    )));
                    // Connection succeeded — persist it (with password) so it
                    // can be reconnected by double-click without re-entering.
                    self.remember_session();
                }
                VncEvent::FramebufferRects(rects) => {
                    // Apply each changed rectangle as a partial texture update —
                    // no full-frame re-upload.
                    if let Some(tex) = &mut self.texture {
                        for r in rects {
                            // Guard against a rect that would fall outside the
                            // current texture (e.g. an update racing a resize),
                            // which would otherwise panic in set_partial.
                            if r.w == 0
                                || r.h == 0
                                || r.x + r.w > self.fb_width
                                || r.y + r.h > self.fb_height
                                || (r.w * r.h * 4) as usize != r.rgba.len()
                            {
                                continue;
                            }
                            let image = egui::ColorImage::from_rgba_unmultiplied(
                                [r.w as usize, r.h as usize],
                                &r.rgba,
                            );
                            tex.set_partial(
                                [r.x as usize, r.y as usize],
                                image,
                                TextureOptions::LINEAR,
                            );
                        }
                    }
                    ctx.request_repaint();
                }
                VncEvent::ClipboardText(text) => {
                    if let Some(cb) = &mut self.clipboard {
                        let _ = cb.set_text(text);
                    }
                }
                VncEvent::NeedPassword => {
                    self.status_msg = "Waiting for password…".to_string();
                    self.pending_password.clear();
                    self.show_password_dialog = true;
                }
                VncEvent::Error(e) => {
                    self.status_msg = format!("Error: {e}");
                    self.connection = None;
                    self.texture = None;
                    self.show_password_dialog = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                        "IronVNC".to_string(),
                    ));
                }
                VncEvent::Disconnected => {
                    self.status_msg = "Disconnected".to_string();
                    self.connection = None;
                    self.texture = None;
                    self.show_password_dialog = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                        "IronVNC".to_string(),
                    ));
                }
            }
        }
    }

    // ── SFTP transfer events ──────────────────────────────────────────────────

    fn process_transfer_events(&mut self) {
        let events: Vec<TransferEvent> = match &self.transfer {
            Some(t) => t.event_rx.try_iter().collect(),
            None => return,
        };
        for event in events {
            match event {
                TransferEvent::Connected { home } => {
                    self.remote_path = home;
                    self.transfer_status = "Connected".to_string();
                }
                TransferEvent::DirListing { path, entries } => {
                    self.remote_path = path;
                    self.remote_entries = entries;
                    self.remote_selected = None;
                }
                TransferEvent::Status(s) => self.transfer_status = s,
                TransferEvent::TransferDone { name } => {
                    self.transfer_status = format!("Done: {name}");
                }
                TransferEvent::Error(e) => {
                    self.transfer_status = format!("Error: {e}");
                }
                TransferEvent::Disconnected => {
                    if self.transfer.is_some() {
                        self.transfer_status = "SFTP disconnected".to_string();
                    }
                    self.transfer = None;
                }
            }
        }
    }

    // ── SFTP file browser panel (MobaXterm-style) ─────────────────────────────

    fn render_transfer_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("📁 Remote files");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Close panel").clicked() {
                    self.show_transfer = false;
                }
            });
        });
        ui.separator();

        if self.transfer.is_none() {
            self.render_ssh_login(ui);
            return;
        }

        // Path bar + navigation
        ui.horizontal(|ui| {
            if ui.small_button("⬆").on_hover_text("Parent directory").clicked() {
                if let Some(parent) = parent_remote(&self.remote_path) {
                    if let Some(t) = &self.transfer {
                        t.list_dir(parent);
                    }
                }
            }
            if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                let p = self.remote_path.clone();
                if let Some(t) = &self.transfer {
                    t.list_dir(p);
                }
            }
            if ui.small_button("＋dir").on_hover_text("New folder").clicked() {
                let base = self.remote_path.clone();
                let newdir = join_remote(&base, "new_folder");
                if let Some(t) = &self.transfer {
                    t.mkdir(newdir);
                }
            }
        });
        ui.add(egui::Label::new(egui::RichText::new(&self.remote_path).small().monospace()).truncate());
        ui.separator();

        // Entry list
        let mut navigate: Option<String> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 90.0)
            .show(ui, |ui| {
                let entries = self.remote_entries.clone();
                for (i, e) in entries.iter().enumerate() {
                    let icon = if e.is_dir { "📁" } else { "📄" };
                    let label = if e.is_dir {
                        format!("{icon} {}", e.name)
                    } else {
                        format!("{icon} {}  ({})", e.name, human_size(e.size))
                    };
                    let resp =
                        ui.selectable_label(self.remote_selected == Some(i), label);
                    if resp.clicked() {
                        self.remote_selected = Some(i);
                    }
                    if resp.double_clicked() && e.is_dir {
                        navigate = Some(join_remote(&self.remote_path, &e.name));
                    }
                }
            });
        if let Some(path) = navigate {
            if let Some(t) = &self.transfer {
                t.list_dir(path);
            }
        }

        ui.separator();
        // Upload / download controls
        ui.horizontal(|ui| {
            if ui.button("⬆ Upload…").on_hover_text("Pick local files to upload here").clicked() {
                if let Some(files) = rfd::FileDialog::new().pick_files() {
                    let dir = self.remote_path.clone();
                    if let Some(t) = &self.transfer {
                        for f in files {
                            t.upload(f, dir.clone());
                        }
                    }
                }
            }
            let can_dl = self
                .remote_selected
                .and_then(|i| self.remote_entries.get(i))
                .map(|e| !e.is_dir)
                .unwrap_or(false);
            if ui
                .add_enabled(can_dl, egui::Button::new("⬇ Download…"))
                .clicked()
            {
                if let Some(i) = self.remote_selected {
                    if let Some(e) = self.remote_entries.get(i).cloned() {
                        let default_dir = dirs::download_dir()
                            .unwrap_or_else(|| std::path::PathBuf::from("."));
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_directory(&default_dir)
                            .pick_folder()
                        {
                            let remote = join_remote(&self.remote_path, &e.name);
                            if let Some(t) = &self.transfer {
                                t.download(remote, dir);
                            }
                        }
                    }
                }
            }
        });

        ui.add_space(2.0);
        ui.add(
            egui::Label::new(egui::RichText::new(&self.transfer_status).small().weak()).truncate(),
        );
        ui.label(egui::RichText::new("Tip: drag files onto the window to upload").small().weak());
    }

    fn render_ssh_login(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(format!("Connect to {} over SSH (port 22)", self.current_host));
        ui.label(
            egui::RichText::new("Use the machine's OS login — not the VNC password.")
                .small()
                .weak(),
        );
        ui.add_space(6.0);
        egui::Grid::new("ssh_login")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Username");
                ui.text_edit_singleline(&mut self.ssh_user);
                ui.end_row();
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(&mut self.ssh_pass).password(true));
                ui.end_row();
            });
        ui.add_space(6.0);
        let can = !self.ssh_user.trim().is_empty() && !self.current_host.is_empty();
        if ui
            .add_enabled(can, egui::Button::new("Connect SFTP"))
            .clicked()
        {
            let user = self.ssh_user.trim().to_string();
            self.transfer = Some(TransferSession::connect(TransferParams {
                host: self.current_host.clone(),
                port: 22,
                username: user.clone(),
                password: self.ssh_pass.clone(),
            }));
            self.transfer_status = "Connecting…".to_string();
            self.ssh_pass.clear();
            // Remember the SSH username on the matching saved session.
            let host = self.current_host.clone();
            if let Some(s) = self.sessions.iter_mut().find(|s| s.host == host) {
                if s.ssh_user != user {
                    s.ssh_user = user;
                    let _ = sessions::save(&self.sessions);
                }
            }
        }
        if !self.transfer_status.is_empty() {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(egui::RichText::new(&self.transfer_status).small().weak())
                    .truncate(),
            );
        }
    }

    // ── Toolbar ───────────────────────────────────────────────────────────────

    fn render_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if self.connection.is_some() {
                if ui.button("Disconnect").clicked() {
                    self.disconnect(ctx);
                }
                if ui
                    .button("Paste →VNC")
                    .on_hover_text("Send clipboard text to VNC server")
                    .clicked()
                {
                    let text = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok());
                    if let (Some(text), Some(conn)) = (text, &self.connection) {
                        conn.send_clipboard(text);
                    }
                }
                if ui
                    .button("Ctrl+Alt+Del")
                    .on_hover_text("Send Ctrl+Alt+Del (wakes a blank/locked Windows screen)")
                    .clicked()
                {
                    if let Some(conn) = &self.connection {
                        // Press Ctrl, Alt, Delete then release in reverse order.
                        conn.send_key(true, 0xFFE3);
                        conn.send_key(true, 0xFFE9);
                        conn.send_key(true, 0xFFFF);
                        conn.send_key(false, 0xFFFF);
                        conn.send_key(false, 0xFFE9);
                        conn.send_key(false, 0xFFE3);
                    }
                }
                let files_label = if self.show_transfer { "Files ◀" } else { "Files ▶" };
                if ui
                    .button(files_label)
                    .on_hover_text("SFTP file browser over SSH (drag & drop to upload)")
                    .clicked()
                {
                    self.show_transfer = !self.show_transfer;
                }
                if ui.button("Sessions").clicked() {
                    self.show_sessions = !self.show_sessions;
                }
            } else {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.address_input)
                        .hint_text("host  or  host:port")
                        .desired_width(220.0),
                );

                let want_connect = (resp.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter)))
                    || ui
                        .add_enabled(
                            !self.address_input.trim().is_empty(),
                            egui::Button::new("Connect"),
                        )
                        .clicked();

                if want_connect && !self.address_input.trim().is_empty() {
                    let (host, port) = Self::parse_address(&self.address_input);
                    // Open the connection in its own window (this window stays a
                    // launcher, so multiple sessions run side by side).
                    Self::open_host_window(&host, port);
                    self.address_input.clear();
                }

                if ui.button("Sessions").clicked() {
                    self.show_sessions = !self.show_sessions;
                }
            }
        });
    }

    // ── Password dialog (shown only when server requests VNCAuth) ─────────────

    fn render_password_dialog(&mut self, ctx: &egui::Context) {
        let mut wants_ok = false;
        let mut wants_cancel = false;

        egui::Window::new("Password Required")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(280.0);
                ui.label("This server requires a password.");
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label("Password:");
                    let pw_resp = ui.add(
                        egui::TextEdit::singleline(&mut self.pending_password)
                            .password(true)
                            .desired_width(180.0),
                    );
                    // Focus once on first show
                    if !pw_resp.has_focus() && !pw_resp.gained_focus() {
                        pw_resp.request_focus();
                    }
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    wants_ok = ui.button("OK").clicked()
                        || ui.input(|i| i.key_pressed(Key::Enter));
                    wants_cancel = ui.button("Cancel").clicked();
                });
            });

        if wants_ok {
            let pw = self.pending_password.clone();
            // Remember the password so a successful connect can be auto-saved.
            self.current_password = pw.clone();
            if let Some(conn) = &self.connection {
                conn.provide_password(pw);
            }
            self.show_password_dialog = false;
            self.status_msg = "Authenticating…".to_string();
        } else if wants_cancel {
            self.disconnect(ctx);
        }
    }

    /// Persist the current connection (host/port/password) so relaunching or
    /// reconnecting doesn't require re-entering it. Adds a new saved session or
    /// updates the password on an existing one. Called once a connection is
    /// established (so we never save a host that failed to connect).
    fn remember_session(&mut self) {
        if self.current_host.is_empty() {
            return;
        }
        let host = self.current_host.clone();
        let port = self.current_port;
        let pw = self.current_password.clone();

        if let Some(s) = self
            .sessions
            .iter_mut()
            .find(|s| s.host == host && s.port == port)
        {
            // Update a stored password only when we now have a (better) one.
            if !pw.is_empty() && s.password != pw {
                s.password = pw;
                let _ = sessions::save(&self.sessions);
            }
        } else {
            self.sessions.push(Session {
                name: String::new(),
                host,
                port,
                password: pw,
                ssh_user: String::new(),
            });
            let _ = sessions::save(&self.sessions);
        }
    }

    // ── Sessions popup window ─────────────────────────────────────────────────

    fn render_sessions_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_sessions;
        let mut connect_idx: Option<usize> = None;
        let mut delete_idx: Option<usize> = None;
        let mut select_idx: Option<usize> = None;

        let session_data: Vec<(usize, String, bool)> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.display_label(), self.session_selected == Some(i)))
            .collect();

        egui::Window::new("Saved Sessions")
            .collapsible(false)
            .resizable(true)
            .default_width(340.0)
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        if session_data.is_empty() {
                            ui.label("No sessions yet — fill in the form below.");
                        }
                        for (i, label, selected) in &session_data {
                            let i = *i;
                            ui.horizontal(|ui| {
                                let resp = ui.selectable_label(*selected, label.as_str());
                                if resp.double_clicked() {
                                    connect_idx = Some(i);
                                } else if resp.clicked() {
                                    select_idx = Some(i);
                                }
                                if ui.small_button("▶").on_hover_text("Connect").clicked() {
                                    connect_idx = Some(i);
                                }
                                if ui.small_button("✕").on_hover_text("Delete").clicked() {
                                    delete_idx = Some(i);
                                }
                            });
                        }
                    });

                ui.separator();

                ui.horizontal(|ui| {
                    if ui.button("+ New").clicked() {
                        self.session_selected = None;
                        self.sf_name.clear();
                        self.sf_host.clear();
                        self.sf_port = "5900".to_string();
                        self.sf_password.clear();
                        self.sf_editing = None;
                    }
                });

                ui.label(if self.sf_editing.is_some() {
                    "Edit session"
                } else {
                    "New session"
                });

                egui::Grid::new("sf")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut self.sf_name);
                        ui.end_row();
                        ui.label("Host");
                        ui.text_edit_singleline(&mut self.sf_host);
                        ui.end_row();
                        ui.label("Port");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.sf_port)
                                .desired_width(70.0),
                        );
                        ui.end_row();
                        ui.label("Password");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.sf_password)
                                .password(true),
                        );
                        ui.end_row();
                    });

                ui.horizontal(|ui| {
                    let can = !self.sf_host.is_empty();
                    if ui.add_enabled(can, egui::Button::new("Save")).clicked() {
                        self.save_session_form();
                    }
                    if ui
                        .add_enabled(can, egui::Button::new("Save & Connect"))
                        .clicked()
                    {
                        self.save_session_form();
                        if let Some(i) = self.session_selected {
                            connect_idx = Some(i);
                        }
                    }
                });
            });

        self.show_sessions = open;

        if let Some(i) = select_idx {
            self.session_selected = Some(i);
            if let Some(s) = self.sessions.get(i) {
                self.sf_name = s.name.clone();
                self.sf_host = s.host.clone();
                self.sf_port = s.port.to_string();
                self.sf_password = s.password.clone();
                self.sf_editing = Some(i);
            }
        }
        if let Some(i) = delete_idx {
            self.sessions.remove(i);
            if self.session_selected == Some(i) {
                self.session_selected = None;
                self.sf_editing = None;
            }
            let _ = sessions::save(&self.sessions);
        }
        if let Some(i) = connect_idx {
            if let Some(s) = self.sessions.get(i).cloned() {
                // Open each saved session in its own window.
                Self::open_session_window(&s);
            }
        }
    }

    fn save_session_form(&mut self) {
        // Preserve a previously-saved SSH username when editing.
        let ssh_user = self
            .sf_editing
            .and_then(|i| self.sessions.get(i))
            .map(|s| s.ssh_user.clone())
            .unwrap_or_default();
        let session = Session {
            name: self.sf_name.trim().to_string(),
            host: self.sf_host.trim().to_string(),
            port: self.sf_port.parse().unwrap_or(5900),
            password: self.sf_password.clone(),
            ssh_user,
        };
        if let Some(i) = self.sf_editing {
            if i < self.sessions.len() {
                self.sessions[i] = session;
            }
        } else {
            self.sessions.push(session);
            let last = self.sessions.len() - 1;
            self.session_selected = Some(last);
            self.sf_editing = Some(last);
        }
        let _ = sessions::save(&self.sessions);
    }

    // ── Start screen ──────────────────────────────────────────────────────────

    fn render_start_screen(&mut self, ui: &mut egui::Ui) {
        let mut connect_idx: Option<usize> = None;

        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.heading("IronVNC");
            ui.add_space(6.0);

            if self.sessions.is_empty() {
                ui.label("Enter a host above and press Connect. Connections are saved here automatically.");
                ui.add_space(16.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "Sessions file: {}",
                            sessions::display_path()
                        ))
                        .small()
                        .weak(),
                    )
                    .truncate(),
                );
                return;
            }

            ui.label(
                egui::RichText::new("Saved sessions — click to open in a new window")
                    .strong(),
            );
            ui.add_space(10.0);

            let session_data: Vec<(usize, String)> = self
                .sessions
                .iter()
                .enumerate()
                .map(|(i, s)| (i, s.display_label()))
                .collect();

            // Large, single-click session buttons (centered by the parent).
            for (i, label) in session_data {
                let btn = egui::Button::new(
                    egui::RichText::new(format!("🖥  {label}")).size(15.0),
                )
                .min_size(egui::vec2(420.0, 36.0));
                if ui.add(btn).on_hover_text("Open in a new window").clicked() {
                    connect_idx = Some(i);
                }
                ui.add_space(6.0);
            }

            ui.add_space(10.0);
            if ui
                .button("⚙  Manage sessions")
                .on_hover_text("Add, edit, or delete saved sessions")
                .clicked()
            {
                self.show_sessions = true;
            }

            ui.add_space(20.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("Sessions file: {}", sessions::display_path()))
                        .small()
                        .weak(),
                )
                .truncate(),
            );
        });

        if let Some(i) = connect_idx {
            if let Some(s) = self.sessions.get(i).cloned() {
                Self::open_session_window(&s);
            }
        }
    }

    // ── Framebuffer ───────────────────────────────────────────────────────────

    fn render_framebuffer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(texture) = &self.texture else {
            ui.centered_and_justified(|ui| {
                ui.label("Waiting for first frame…");
            });
            return;
        };

        let tex_size = texture.size_vec2();
        let avail = ui.available_size();
        // Scale to fit the window preserving aspect ratio (like RealVNC's
        // "scale to window"), up or down, then center within the available area.
        let scale = (avail.x / tex_size.x).min(avail.y / tex_size.y).max(0.01);
        let display_size = tex_size * scale;

        let (area, response) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        let rect = Rect::from_center_size(area.center(), display_size);

        // Give the remote screen keyboard focus so keystrokes — including Tab,
        // the arrow keys, and Escape — are sent to the server instead of driving
        // egui's own widget navigation. Grab focus on click, and auto-grab when
        // connected and nothing else (e.g. a dialog's text field) owns it.
        if response.clicked() {
            response.request_focus();
        } else if self.connection.is_some() && ctx.memory(|m| m.focused().is_none()) {
            response.request_focus();
        }
        let fb_focused = response.has_focus();
        if fb_focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        ui.painter().image(
            texture.id(),
            rect,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );

        // Distinct cursor while hovering the remote screen, so it's obvious that
        // input is being captured by the VNC session. Also paint a small
        // software cursor marker at the pointer so it's always visible even if
        // the server isn't rendering its own cursor into the framebuffer.
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            if let Some(p) = ui.ctx().input(|i| i.pointer.latest_pos()) {
                if rect.contains(p) {
                    let painter = ui.painter();
                    let c = Color32::from_rgb(0, 200, 255);
                    painter.line_segment([p - egui::vec2(7.0, 0.0), p + egui::vec2(7.0, 0.0)],
                        egui::Stroke::new(1.5_f32, c));
                    painter.line_segment([p - egui::vec2(0.0, 7.0), p + egui::vec2(0.0, 7.0)],
                        egui::Stroke::new(1.5_f32, c));
                    painter.circle_stroke(p, 3.0, egui::Stroke::new(1.0_f32, c));
                }
            }
        }

        let Some(conn) = &self.connection else { return };

        // ── Mouse ──
        let pointer_pos = response.interact_pointer_pos().or_else(|| {
            ctx.input(|i| i.pointer.latest_pos())
                .filter(|p| rect.contains(*p))
        });

        if let Some(pos) = pointer_pos {
            let local = pos - rect.min;
            let nx = (local.x / scale).clamp(0.0, self.fb_width as f32 - 1.0) as u16;
            let ny = (local.y / scale).clamp(0.0, self.fb_height as f32 - 1.0) as u16;
            self.last_mouse_pos = (nx, ny);

            let mut buttons = 0u8;
            if response.dragged_by(egui::PointerButton::Primary)
                || response.is_pointer_button_down_on()
            {
                buttons |= 0x01;
            }
            if ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle)) {
                buttons |= 0x02;
            }
            if ctx.input(|i| i.pointer.button_down(egui::PointerButton::Secondary)) {
                buttons |= 0x04;
            }
            conn.send_pointer(buttons, nx, ny);
        }

        // ── Scroll wheel ──
        let (mx, my) = self.last_mouse_pos;
        ctx.input(|i| {
            let scroll = i.raw_scroll_delta;
            if rect.contains(i.pointer.latest_pos().unwrap_or(egui::Pos2::ZERO)) {
                if scroll.y > 0.0 {
                    conn.send_pointer(0x08, mx, my);
                    conn.send_pointer(0x00, mx, my);
                } else if scroll.y < 0.0 {
                    conn.send_pointer(0x10, mx, my);
                    conn.send_pointer(0x00, mx, my);
                }
            }
        });

        // ── Keyboard (only when the remote screen owns focus, so typing in the
        //    Sessions form or other UI fields doesn't leak to the server) ──
        if fb_focused {
            let new_mods = ctx.input(|i| i.modifiers);
            if new_mods != self.last_modifiers {
                if new_mods.ctrl && !self.last_modifiers.ctrl {
                    conn.send_key(true, 0xffe3);
                } else if !new_mods.ctrl && self.last_modifiers.ctrl {
                    conn.send_key(false, 0xffe3);
                }
                if new_mods.shift && !self.last_modifiers.shift {
                    conn.send_key(true, 0xffe1);
                } else if !new_mods.shift && self.last_modifiers.shift {
                    conn.send_key(false, 0xffe1);
                }
                if new_mods.alt && !self.last_modifiers.alt {
                    conn.send_key(true, 0xffe9);
                } else if !new_mods.alt && self.last_modifiers.alt {
                    conn.send_key(false, 0xffe9);
                }
                self.last_modifiers = new_mods;
            }

            ctx.input(|i| {
                for event in &i.events {
                    match event {
                        egui::Event::Key { key, pressed, .. } => {
                            let sym = key_to_keysym(*key);
                            if sym != 0 {
                                conn.send_key(*pressed, sym);
                            }
                        }
                        egui::Event::Text(text) => {
                            for ch in text.chars() {
                                conn.send_key(true, ch as u32);
                                conn.send_key(false, ch as u32);
                            }
                        }
                        _ => {}
                    }
                }
            });
        } else if self.last_modifiers != Modifiers::default() {
            // Focus left the remote screen while modifiers were held — release
            // them on the server so they don't get stuck down.
            if self.last_modifiers.ctrl {
                conn.send_key(false, 0xffe3);
            }
            if self.last_modifiers.shift {
                conn.send_key(false, 0xffe1);
            }
            if self.last_modifiers.alt {
                conn.send_key(false, 0xffe9);
            }
            self.last_modifiers = Modifiers::default();
        }
    }
}

impl eframe::App for VncApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events(ctx);
        self.process_transfer_events();

        // Drag & drop files onto the window → upload to the current remote dir.
        if self.transfer.is_some() {
            let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect()
            });
            if !dropped.is_empty() {
                let dir = self.remote_path.clone();
                if let Some(t) = &self.transfer {
                    for f in dropped {
                        if f.is_file() {
                            t.upload(f, dir.clone());
                        }
                    }
                }
                self.show_transfer = true;
            }
        }

        // One-time: size window to ~80% of monitor, centered
        if self.first_frame {
            self.first_frame = false;
            if let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) {
                let w = (monitor.x * 0.80).clamp(800.0, 1400.0);
                let h = (monitor.y * 0.80).clamp(560.0, 900.0);
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(w, h)));
                if let Some(center_cmd) = egui::ViewportCommand::center_on_screen(ctx) {
                    ctx.send_viewport_cmd(center_cmd);
                }
            }
        }

        // Floating windows
        if self.show_password_dialog {
            self.render_password_dialog(ctx);
        }
        if self.show_sessions {
            self.render_sessions_window(ctx);
        }

        // Top toolbar
        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(6.0))
            .show(ctx, |ui| {
                self.render_toolbar(ui, ctx);
            });

        // Status bar — truncates instead of overflowing
        egui::TopBottomPanel::bottom("statusbar")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(4.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let color = if self.status_msg.starts_with("Error") {
                        Color32::from_rgb(220, 80, 80)
                    } else if self.connection.is_some() {
                        Color32::from_rgb(100, 210, 100)
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&self.status_msg).color(color).small(),
                        )
                        .truncate(),
                    );
                });
            });

        // SFTP file-browser side panel (only while a VNC session is active)
        if self.show_transfer && self.connection.is_some() {
            egui::SidePanel::right("sftp_panel")
                .resizable(true)
                .default_width(320.0)
                .min_width(260.0)
                .show(ctx, |ui| {
                    self.render_transfer_panel(ui);
                });
        }

        // Main area
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.connection.is_some() || self.texture.is_some() {
                self.render_framebuffer(ui, ctx);
            } else {
                self.render_start_screen(ui);
            }
        });

        if self.connection.is_some() || self.transfer.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
}

/// Join a remote directory and a name with a single POSIX '/'.
fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Parent of a POSIX remote path, or None at the root.
fn parent_remote(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(i) => Some(trimmed[..i].to_string()),
        None => None,
    }
}

/// Human-readable byte size.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn key_to_keysym(key: Key) -> u32 {
    match key {
        Key::Enter => 0xff0d,
        Key::Escape => 0xff1b,
        Key::Backspace => 0xff08,
        Key::Tab => 0xff09,
        Key::ArrowLeft => 0xff51,
        Key::ArrowUp => 0xff52,
        Key::ArrowRight => 0xff53,
        Key::ArrowDown => 0xff54,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::Delete => 0xffff,
        Key::Insert => 0xff63,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
        _ => 0,
    }
}
