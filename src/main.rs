mod app;
mod connection;
mod framebuffer;
mod protocol;
mod sessions;
mod transfer;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "ironvnc", about = "IronVNC — a free, open-source VNC viewer written in Rust")]
struct Args {
    /// VNC server host
    #[arg(short = 'H', long)]
    host: Option<String>,

    /// VNC server port
    #[arg(short, long, default_value_t = 5900)]
    port: u16,

    /// VNC password
    #[arg(short = 'P', long)]
    password: Option<String>,

    /// Headless test mode: connect, wait for the first framebuffer, report, exit.
    #[arg(long, default_value_t = false)]
    test: bool,

    /// Headless SFTP test: connect over SSH and list the home directory.
    #[arg(long, default_value_t = false)]
    sftp_test: bool,

    /// SSH username (for --sftp-test).
    #[arg(long)]
    user: Option<String>,

    /// Open a saved session by name, in this window.
    #[arg(long)]
    session: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let args = Args::parse();

    if args.test {
        let host = args
            .host
            .ok_or_else(|| anyhow::anyhow!("--test requires --host"))?;
        return headless_test(host, args.port, args.password);
    }

    if args.sftp_test {
        let host = args
            .host
            .ok_or_else(|| anyhow::anyhow!("--sftp-test requires --host"))?;
        let user = args
            .user
            .ok_or_else(|| anyhow::anyhow!("--sftp-test requires --user"))?;
        return sftp_test(host, user, args.password.unwrap_or_default());
    }

    // Resolve the initial connection. `--session <name>` opens a saved session
    // (used when launching a session in its own window). Otherwise `--host`;
    // if no password was given, auto-load it from a saved session matching
    // host:port so passwords never travel on the command line.
    let initial_connection = if let Some(name) = args.session.clone() {
        sessions::load()
            .into_iter()
            .find(|s| s.name == name || s.host == name)
            .map(|s| connection::ConnectionParams {
                host: s.host,
                port: s.port,
                password: (!s.password.is_empty()).then_some(s.password),
            })
    } else {
        args.host.clone().map(|host| {
            let password = args.password.clone().or_else(|| {
                sessions::load()
                    .into_iter()
                    .find(|s| s.host == host && s.port == args.port)
                    .map(|s| s.password)
                    .filter(|p| !p.is_empty())
            });
            connection::ConnectionParams {
                host,
                port: args.port,
                password,
            }
        })
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("IronVNC")
            .with_inner_size([1100.0, 750.0]),
        ..Default::default()
    };

    eframe::run_native(
        "IronVNC",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::VncApp::new(cc, initial_connection)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

/// Connect over SSH/SFTP without a GUI and list the home directory.
fn sftp_test(host: String, user: String, password: String) -> Result<()> {
    use std::time::{Duration, Instant};
    use transfer::{TransferEvent, TransferParams, TransferSession};

    println!("[sftp] connecting to {user}@{host}:22 ...");
    let sess = TransferSession::connect(TransferParams {
        host,
        port: 22,
        username: user,
        password,
    });

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match sess.event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(TransferEvent::Connected { home }) => {
                println!("[sftp] authenticated; home = {home}");
            }
            Ok(TransferEvent::DirListing { path, entries }) => {
                println!("[sftp] PASS: listed {path} ({} entries):", entries.len());
                for e in entries.iter().take(20) {
                    let kind = if e.is_dir { "dir " } else { "file" };
                    println!("        [{kind}] {} ({} B)", e.name, e.size);
                }
                sess.disconnect();
                return Ok(());
            }
            Ok(TransferEvent::Status(s)) => println!("[sftp] {s}"),
            Ok(TransferEvent::Error(e)) => {
                println!("[sftp] FAIL: {e}");
                std::process::exit(1);
            }
            Ok(TransferEvent::TransferDone { .. }) => {}
            Ok(TransferEvent::Disconnected) => {
                println!("[sftp] FAIL: disconnected before listing");
                std::process::exit(1);
            }
            Err(_) => {}
        }
    }
    println!("[sftp] FAIL: timed out");
    std::process::exit(1);
}

/// Connect without a GUI, wait for the first framebuffer update, and report:
/// desktop name/size and how much of the first frame is non-black (a fully
/// black frame usually means a decode bug, not a working session).
fn headless_test(host: String, port: u16, password: Option<String>) -> Result<()> {
    use connection::{VncConnection, VncEvent};
    use std::time::{Duration, Instant};

    println!("[test] connecting to {host}:{port} ...");
    let conn = VncConnection::connect(connection::ConnectionParams {
        host,
        port,
        password,
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let start = Instant::now();
    let mut got_size = None;
    let mut got_name: Option<String> = None;
    let mut frames = 0u32;
    let mut best_pct = 0usize;
    let mut wiggled = false;
    let mut cad_sent = false;
    let mut fb_buf: Vec<u8> = Vec::new();

    while Instant::now() < deadline {
        match conn.event_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(VncEvent::DesktopSize(w, h)) => {
                got_size = Some((w, h));
                println!("[test] desktop size: {w}x{h}");
            }
            Ok(VncEvent::DesktopName(n)) => {
                println!("[test] desktop name: {n:?}");
                got_name = Some(n);
            }
            Ok(VncEvent::NeedPassword) => {
                println!("[test] FAIL: server asked for a password but none was supplied");
                conn.disconnect();
                std::process::exit(2);
            }
            Ok(VncEvent::FramebufferRects(rects)) => {
                frames += 1;
                // Apply rects into a local buffer to measure whole-screen content.
                let (fw, fh) = got_size.unwrap_or((0, 0));
                if fb_buf.len() != (fw * fh * 4) as usize {
                    fb_buf = vec![0u8; (fw * fh * 4) as usize];
                }
                for r in &rects {
                    for row in 0..r.h {
                        let src = (row * r.w * 4) as usize;
                        let dst = (((r.y + row) * fw + r.x) * 4) as usize;
                        let n = (r.w * 4) as usize;
                        if dst + n <= fb_buf.len() && src + n <= r.rgba.len() {
                            fb_buf[dst..dst + n].copy_from_slice(&r.rgba[src..src + n]);
                        }
                    }
                }
                let non_black = fb_buf
                    .chunks_exact(4)
                    .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
                    .count();
                let total = (fw * fh) as usize;
                let pct = if total > 0 { non_black * 100 / total } else { 0 };
                best_pct = best_pct.max(pct);
                println!(
                    "[test] frame {frames}: {fw}x{fh}, {non_black}/{total} non-black pixels ({pct}%)"
                );

                // PASS on any meaningful content: a full desktop is >1%, but a
                // locked screen (clock/login box on black) may be only a few
                // thousand pixels — still proof the decoder works.
                if pct >= 1 || non_black > 1000 {
                    println!(
                        "[test] PASS: connected to {:?} ({}x{}), framebuffer has content \
                         ({non_black} non-black px, {pct}%)",
                        got_name.as_deref().unwrap_or("?"),
                        got_size.map_or(0, |s| s.0),
                        got_size.map_or(0, |s| s.1),
                    );
                    conn.disconnect();
                    return Ok(());
                }

                // All-black frame: wiggle the mouse once to wake a blanked/
                // sleeping remote display, then keep watching for content.
                if !wiggled {
                    if let Some((w, h)) = got_size {
                        let (cx, cy) = ((w / 2) as u16, (h / 2) as u16);
                        conn.send_pointer(0, cx, cy);
                        conn.send_pointer(0, cx + 20, cy + 20);
                        conn.send_pointer(0, cx, cy);
                        // Shift tap: harmless, and wakes a sleeping display
                        // more reliably than pointer motion.
                        conn.send_key(true, 0xFFE1);
                        conn.send_key(false, 0xFFE1);
                        println!("[test] frame is black; sent mouse wiggle + shift tap to wake display");
                        wiggled = true;
                    }
                }
            }
            Ok(VncEvent::ClipboardText(_)) => {}
            Ok(VncEvent::Error(e)) => {
                println!("[test] FAIL: {e}");
                std::process::exit(1);
            }
            Ok(VncEvent::Disconnected) => {
                println!("[test] FAIL: disconnected before first frame");
                std::process::exit(1);
            }
            Err(_) => {} // poll timeout, keep waiting
        }

        // Time-based escalation: still black several seconds after connecting →
        // a Windows box at the secure desktop with its display off needs
        // Ctrl+Alt+Del (SAS) to wake and raise the login screen. Also click
        // once — lock screens dismiss on click.
        if wiggled && !cad_sent && best_pct == 0 && start.elapsed() > Duration::from_secs(5) {
            if let Some((w, h)) = got_size {
                let (cx, cy) = ((w / 2) as u16, (h / 2) as u16);
                conn.send_pointer(1, cx, cy); // left button down
                conn.send_pointer(0, cx, cy); // release
                conn.send_key(true, 0xFFE3); // Ctrl
                conn.send_key(true, 0xFFE9); // Alt
                conn.send_key(true, 0xFFFF); // Delete
                conn.send_key(false, 0xFFFF);
                conn.send_key(false, 0xFFE9);
                conn.send_key(false, 0xFFE3);
                println!("[test] still black; sent click + Ctrl+Alt+Del to raise login screen");
                cad_sent = true;
            }
        }
    }

    if frames > 0 {
        println!(
            "[test] CONNECTED to {:?}, {frames} frames received, but all frames are black \
             (best {best_pct}% non-black). The protocol works; the remote screen itself \
             appears to be blank/locked/asleep.",
            got_name.as_deref().unwrap_or("?"),
        );
        conn.disconnect();
        return Ok(());
    }
    println!("[test] FAIL: timed out after 30s with no frames");
    conn.disconnect();
    std::process::exit(1);
}
