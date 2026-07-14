use crate::connection::{ConnectionParams, FbRect, VncCommand, VncEvent};
use crate::framebuffer::Framebuffer;
use crate::protocol::{
    encoding::{copyrect, hextile, raw, tight, zrle},
    messages::{encoding as enc, server_msg, PixelFormat},
    security,
};
use anyhow::{bail, Result};
use flate2::Decompress;
use crossbeam_channel::{Receiver, Sender};
use tight::TightState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};

pub struct VncClient;

impl VncClient {
    pub async fn connect(
        params: &ConnectionParams,
        event_tx: Sender<VncEvent>,
        cmd_rx: Receiver<VncCommand>,
    ) -> Result<()> {
        let addr = format!("{}:{}", params.host, params.port);
        info!("Connecting to {addr}");
        // Keep the raw TcpStream (no BufReader) through the security handshake:
        // RA2 needs to take ownership of it, and a BufReader could over-read
        // handshake bytes into a buffer that would then be lost.
        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            TcpStream::connect(&addr),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Connection timed out after 10 seconds"))??;

        // --- Protocol version handshake ---
        let mut ver_buf = [0u8; 12];
        stream.read_exact(&mut ver_buf).await?;
        let ver_str = std::str::from_utf8(&ver_buf)?;
        debug!("Server version: {ver_str:?}");

        // Parse "RFB xxx.yyy" numerically. Reply with the highest handshake we
        // support that does not exceed the server's — capping at 3.8. Crucially,
        // an UNKNOWN-but-HIGHER version (RealVNC "003.889", "004.001", etc.) must
        // NOT downgrade to 3.3: servers that require VncAuth/VeNCrypt refuse to
        // offer any security type over the legacy 3.3 handshake.
        let (srv_major, srv_minor) = parse_rfb_version(ver_str);
        let rfb_minor: u8 = if srv_major > 3 || (srv_major == 3 && srv_minor >= 8) {
            8
        } else if srv_major == 3 && srv_minor == 7 {
            7
        } else {
            3
        };

        let reply = format!("RFB 003.00{}\n", rfb_minor);
        info!("Server offered {ver_str:?}; replying RFB 003.00{rfb_minor}");
        stream.write_all(reply.as_bytes()).await?;

        // --- Security (may replace the stream with an AES-encrypted channel) ---
        let mut stream = security::negotiate(
            stream,
            params.password.as_deref(),
            rfb_minor,
            &event_tx,
            &cmd_rx,
        )
        .await?;

        // --- ClientInit ---
        stream.write_u8(1).await?;

        // --- ServerInit ---
        let fb_width = stream.read_u16().await?;
        let fb_height = stream.read_u16().await?;

        let bpp = stream.read_u8().await?;
        let depth = stream.read_u8().await?;
        let big_endian = stream.read_u8().await? != 0;
        let true_colour = stream.read_u8().await? != 0;
        let red_max = stream.read_u16().await?;
        let green_max = stream.read_u16().await?;
        let blue_max = stream.read_u16().await?;
        let red_shift = stream.read_u8().await?;
        let green_shift = stream.read_u8().await?;
        let blue_shift = stream.read_u8().await?;
        stream.read_exact(&mut [0u8; 3]).await?;

        let name_len = stream.read_u32().await?;
        let mut name_bytes = vec![0u8; name_len as usize];
        stream.read_exact(&mut name_bytes).await?;
        let name = String::from_utf8_lossy(&name_bytes).to_string();

        info!("Desktop: {name} ({fb_width}x{fb_height})");
        info!(
            "Server pixel format: {bpp}bpp depth={depth} big_endian={big_endian} \
             true_colour={true_colour} rgb_max=({red_max},{green_max},{blue_max}) \
             rgb_shift=({red_shift},{green_shift},{blue_shift})"
        );

        let pf = PixelFormat {
            bits_per_pixel: bpp,
            depth,
            big_endian,
            true_colour,
            red_max,
            green_max,
            blue_max,
            red_shift,
            green_shift,
            blue_shift,
        };

        let _ = event_tx.send(VncEvent::DesktopSize(fb_width as u32, fb_height as u32));
        let _ = event_tx.send(VncEvent::DesktopName(name));

        // --- SetEncodings (prefer ZRLE > Hextile > CopyRect > Raw; Tight last) ---
        let encodings: &[i32] = &[
            enc::ZRLE,
            enc::HEXTILE,
            enc::COPY_RECT,
            enc::RAW,
            enc::DESKTOP_SIZE,
            enc::TIGHT,
        ];
        let mut enc_msg = Vec::with_capacity(4 + encodings.len() * 4);
        enc_msg.push(crate::protocol::messages::client_msg::SET_ENCODINGS);
        enc_msg.push(0);
        enc_msg.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
        for &e in encodings {
            enc_msg.extend_from_slice(&e.to_be_bytes());
        }
        stream.write_all(&enc_msg).await?;

        // --- Initial full FramebufferUpdateRequest ---
        send_fb_update_request(&mut stream, false, 0, 0, fb_width, fb_height).await?;

        let mut fb = Framebuffer::new(fb_width as u32, fb_height as u32);
        let mut zrle_decomp = Decompress::new(true);
        let mut tight_state = TightState::new();

        // --- Main message loop ---
        loop {
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    VncCommand::Disconnect => return Ok(()),
                    VncCommand::KeyEvent { down, keysym } => {
                        send_key_event(&mut stream, down, keysym).await?;
                    }
                    VncCommand::PointerEvent { buttons, x, y } => {
                        send_pointer_event(&mut stream, buttons, x, y).await?;
                    }
                    VncCommand::SetClipboard(text) => {
                        send_client_cut_text(&mut stream, &text).await?;
                    }
                    // ProvidePassword is only consumed during security negotiation;
                    // ignore any stray commands that arrive after auth completes.
                    VncCommand::ProvidePassword(_) => {}
                }
            }

            // Wait for the next server message, but only briefly: a VNC server
            // replies to an incremental update request only when the screen
            // changes, so on a static screen this read would block forever and
            // starve the command drain above (input never gets sent → deadlock).
            // Time out quickly and loop back to flush any pending input; that
            // input reaches the server, the screen changes, and updates resume.
            // read_u8 goes through a BufReader, so a timed-out read consumes no
            // bytes and is safe to retry.
            let msg_type = match tokio::time::timeout(
                std::time::Duration::from_millis(5),
                stream.read_u8(),
            )
            .await
            {
                Ok(r) => r?,
                Err(_) => continue,
            };
            match msg_type {
                server_msg::FB_UPDATE => {
                    stream.read_u8().await?;
                    let n_rects = stream.read_u16().await?;

                    // Collect the bounds of each drawn rectangle so we can send
                    // only those regions to the UI (partial texture updates).
                    let mut dirty: Vec<(u16, u16, u16, u16)> = Vec::with_capacity(n_rects as usize);

                    for _ in 0..n_rects {
                        let rx = stream.read_u16().await?;
                        let ry = stream.read_u16().await?;
                        let rw = stream.read_u16().await?;
                        let rh = stream.read_u16().await?;
                        let encoding = stream.read_i32().await?;

                        match encoding {
                            enc::RAW => {
                                raw::decode(&mut stream, &mut fb, &pf, rx, ry, rw, rh).await?;
                                dirty.push((rx, ry, rw, rh));
                            }
                            enc::COPY_RECT => {
                                copyrect::decode(&mut stream, &mut fb, rx, ry, rw, rh).await?;
                                dirty.push((rx, ry, rw, rh));
                            }
                            enc::ZRLE => {
                                zrle::decode(
                                    &mut stream, &mut fb, &pf, rx, ry, rw, rh,
                                    &mut zrle_decomp,
                                ).await?;
                                dirty.push((rx, ry, rw, rh));
                            }
                            enc::HEXTILE => {
                                hextile::decode(
                                    &mut stream, &mut fb, &pf, rx, ry, rw, rh,
                                ).await?;
                                dirty.push((rx, ry, rw, rh));
                            }
                            enc::TIGHT => {
                                tight::decode(
                                    &mut stream, &mut fb, &pf, rx, ry, rw, rh,
                                    &mut tight_state,
                                ).await?;
                                dirty.push((rx, ry, rw, rh));
                            }
                            enc::DESKTOP_SIZE => {
                                fb.resize(rw as u32, rh as u32);
                                let _ = event_tx.send(VncEvent::DesktopSize(rw as u32, rh as u32));
                                send_fb_update_request(&mut stream, false, 0, 0, rw, rh).await?;
                            }
                            enc::CURSOR => {
                                // RichCursor pseudo-encoding: read and discard cursor image + bitmask.
                                let bpp = (pf.bits_per_pixel / 8) as usize;
                                let img_bytes = rw as usize * rh as usize * bpp;
                                let mask_bytes = ((rw as usize + 7) / 8) * rh as usize;
                                let mut skip = vec![0u8; img_bytes + mask_bytes];
                                stream.read_exact(&mut skip).await?;
                            }
                            other => {
                                bail!("Unsupported encoding {other} — cannot safely continue (stream would desync)");
                            }
                        }
                    }

                    if !dirty.is_empty() {
                        let rects: Vec<FbRect> = dirty
                            .into_iter()
                            .map(|(rx, ry, rw, rh)| {
                                let (w, h, rgba) =
                                    fb.sub_rgba(rx as u32, ry as u32, rw as u32, rh as u32);
                                FbRect { x: rx as u32, y: ry as u32, w, h, rgba }
                            })
                            .collect();
                        let _ = event_tx.send(VncEvent::FramebufferRects(rects));
                    }

                    send_fb_update_request(
                        &mut stream, true, 0, 0, fb.width as u16, fb.height as u16,
                    ).await?;
                }

                server_msg::SET_COLOUR_MAP_ENTRIES => {
                    stream.read_u8().await?;
                    let _first = stream.read_u16().await?;
                    let count = stream.read_u16().await?;
                    let mut skip = vec![0u8; count as usize * 6];
                    stream.read_exact(&mut skip).await?;
                }

                server_msg::BELL => {
                    debug!("Bell!");
                }

                server_msg::SERVER_CUT_TEXT => {
                    stream.read_exact(&mut [0u8; 3]).await?;
                    let len = stream.read_u32().await?;
                    let mut text_bytes = vec![0u8; len as usize];
                    stream.read_exact(&mut text_bytes).await?;
                    let text = String::from_utf8_lossy(&text_bytes).to_string();
                    debug!("Server clipboard: {} chars", text.len());
                    let _ = event_tx.send(VncEvent::ClipboardText(text));
                }

                other => {
                    bail!("Unknown server message type: {other}");
                }
            }
        }
    }
}

/// Parse an RFB version banner ("RFB xxx.yyy\n") into (major, minor).
/// Falls back to (3, 3) only when the banner is genuinely unparseable.
fn parse_rfb_version(ver: &str) -> (u32, u32) {
    // Expected exact form: "RFB " + 3 digits + '.' + 3 digits + '\n' (12 bytes).
    // Use byte-safe slicing: a hostile server can send a valid multi-byte UTF-8
    // banner where a char straddles the slice boundary, which would panic on
    // `ver[8..11]`. `get()` returns None instead of panicking.
    if ver.starts_with("RFB ") && ver.as_bytes().get(7) == Some(&b'.') {
        if let (Some(maj_s), Some(min_s)) = (ver.get(4..7), ver.get(8..11)) {
            if let (Ok(maj), Ok(min)) = (maj_s.parse::<u32>(), min_s.parse::<u32>()) {
                return (maj, min);
            }
        }
    }
    (3, 3)
}

async fn send_fb_update_request<S>(
    stream: &mut S,
    incremental: bool,
    x: u16, y: u16, w: u16, h: u16,
) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let mut msg = [0u8; 10];
    msg[0] = crate::protocol::messages::client_msg::FB_UPDATE_REQUEST;
    msg[1] = if incremental { 1 } else { 0 };
    msg[2..4].copy_from_slice(&x.to_be_bytes());
    msg[4..6].copy_from_slice(&y.to_be_bytes());
    msg[6..8].copy_from_slice(&w.to_be_bytes());
    msg[8..10].copy_from_slice(&h.to_be_bytes());
    stream.write_all(&msg).await?;
    Ok(())
}

async fn send_key_event<S>(stream: &mut S, down: bool, keysym: u32) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let mut msg = [0u8; 8];
    msg[0] = crate::protocol::messages::client_msg::KEY_EVENT;
    msg[1] = if down { 1 } else { 0 };
    msg[4..8].copy_from_slice(&keysym.to_be_bytes());
    stream.write_all(&msg).await?;
    Ok(())
}

async fn send_pointer_event<S>(stream: &mut S, buttons: u8, x: u16, y: u16) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let mut msg = [0u8; 6];
    msg[0] = crate::protocol::messages::client_msg::POINTER_EVENT;
    msg[1] = buttons;
    msg[2..4].copy_from_slice(&x.to_be_bytes());
    msg[4..6].copy_from_slice(&y.to_be_bytes());
    stream.write_all(&msg).await?;
    Ok(())
}

async fn send_client_cut_text<S>(stream: &mut S, text: &str) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    let bytes = text.as_bytes();
    let mut msg = vec![0u8; 8 + bytes.len()];
    msg[0] = crate::protocol::messages::client_msg::CLIENT_CUT_TEXT;
    // msg[1..4] = padding
    msg[4..8].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    msg[8..].copy_from_slice(bytes);
    stream.write_all(&msg).await?;
    Ok(())
}
