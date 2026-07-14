use crate::connection::{VncCommand, VncEvent};
use crate::protocol::{ra2, AsyncRw};
use anyhow::{bail, Result};
use cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use crossbeam_channel::{Receiver, Sender};
use des::Des;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::info;

pub const SECURITY_NONE: u8 = 1;
pub const SECURITY_VNC_AUTH: u8 = 2;
pub const SECURITY_RA2: u8 = 5;
pub const SECURITY_RA2NE: u8 = 6;
pub const SECURITY_RA2_256: u8 = 129;
pub const SECURITY_RA2NE_256: u8 = 130;

/// Negotiate and complete the security handshake, taking ownership of the
/// TCP stream. Returns the stream the RFB session should continue on — the
/// raw TCP stream (buffered) for None/VncAuth/RA2ne, or an AES-EAX encrypted
/// channel for RA2/RA2_256 (where the whole session is encrypted).
///
/// If the server requires auth and no password was supplied, we send
/// `VncEvent::NeedPassword` and block-wait on `cmd_rx` for a
/// `VncCommand::ProvidePassword`.  This is safe because the VNC background
/// thread is the only task on its single-threaded tokio runtime, and blocking
/// it while waiting for user input is harmless.
pub async fn negotiate(
    mut stream: TcpStream,
    password: Option<&str>,
    rfb_minor: u8,
    event_tx: &Sender<VncEvent>,
    cmd_rx: &Receiver<VncCommand>,
) -> Result<Box<dyn AsyncRw>> {
    // Track whether we actually ran VNC Authentication: the SecurityResult
    // message follows VNC Auth in ALL RFB versions (3.3/3.7/3.8), not just 3.8.
    let mut did_vnc_auth = false;

    let chosen: u8 = if rfb_minor >= 7 {
        let count = stream.read_u8().await?;
        if count == 0 {
            let reason = read_reason_string(&mut stream).await.unwrap_or_default();
            bail!("Server rejected connection: {reason}");
        }
        let mut types = vec![0u8; count as usize];
        stream.read_exact(&mut types).await?;
        info!("Server security types: {types:?}");

        // Preference: None (when we have no password) > VncAuth > None >
        // RA2 family (RSA-AES; needed for RealVNC servers).
        let chosen = if types.contains(&SECURITY_NONE) && password.map_or(true, |p| p.is_empty()) {
            SECURITY_NONE
        } else if types.contains(&SECURITY_VNC_AUTH) {
            SECURITY_VNC_AUTH
        } else if types.contains(&SECURITY_NONE) {
            SECURITY_NONE
        } else if types.contains(&SECURITY_RA2) {
            SECURITY_RA2
        } else if types.contains(&SECURITY_RA2_256) {
            SECURITY_RA2_256
        } else if types.contains(&SECURITY_RA2NE) {
            SECURITY_RA2NE
        } else if types.contains(&SECURITY_RA2NE_256) {
            SECURITY_RA2NE_256
        } else {
            bail!(
                "No supported security type offered by server (got {:?}). \
                 This viewer supports None(1), VNC Auth(2), and RA2/RSA-AES(5/6/129/130); \
                 the server likely requires VeNCrypt/ARD, which are not yet implemented.",
                types
            );
        };

        stream.write_u8(chosen).await?;
        chosen
    } else {
        // RFB 3.3: server dictates security type as a single u32.
        let sec_type = stream.read_u32().await?;
        match sec_type {
            1 => SECURITY_NONE,
            2 => SECURITY_VNC_AUTH,
            0 => {
                let reason = read_reason_string(&mut stream).await.unwrap_or_default();
                bail!("Server error: {reason}");
            }
            _ => bail!("Unknown security type: {sec_type}"),
        }
    };

    // --- RA2 family: RSA-AES handshake, possibly wrapping the whole session ---
    if matches!(
        chosen,
        SECURITY_RA2 | SECURITY_RA2_256 | SECURITY_RA2NE | SECURITY_RA2NE_256
    ) {
        let sha256 = matches!(chosen, SECURITY_RA2_256 | SECURITY_RA2NE_256);
        let encrypt_all = matches!(chosen, SECURITY_RA2 | SECURITY_RA2_256);
        info!(
            "Using RA2 (RSA-AES{}, {})",
            if sha256 { "-256" } else { "-128" },
            if encrypt_all { "full encryption" } else { "handshake only" },
        );

        let pw = resolve_password(password, event_tx, cmd_rx)?;
        let (send, recv) = ra2::handshake(&mut stream, "", &pw, sha256).await?;

        if encrypt_all {
            // Everything from SecurityResult onward is inside the AES channel.
            // Bridge it to the client through an in-memory duplex + pump tasks.
            let (local, remote) = tokio::io::duplex(1 << 16);
            let (tcp_r, tcp_w) = tokio::io::split(stream);
            let (rem_r, rem_w) = tokio::io::split(remote);
            tokio::spawn(ra2::pump_decrypt(tcp_r, rem_w, recv));
            tokio::spawn(ra2::pump_encrypt(rem_r, tcp_w, send));

            let mut wrapped: Box<dyn AsyncRw> = Box::new(BufReader::new(local));
            read_security_result(&mut wrapped, rfb_minor >= 8).await?;
            return Ok(wrapped);
        } else {
            // "ne" variants: SecurityResult and the session stay in plaintext.
            let mut plain: Box<dyn AsyncRw> = Box::new(BufReader::new(stream));
            read_security_result(&mut plain, rfb_minor >= 8).await?;
            return Ok(plain);
        }
    }

    // --- Classic types over the plain stream ---
    if chosen == SECURITY_VNC_AUTH {
        let pw = resolve_password(password, event_tx, cmd_rx)?;
        vnc_auth(&mut stream, &pw).await?;
        did_vnc_auth = true;
    }

    let mut plain: Box<dyn AsyncRw> = Box::new(BufReader::new(stream));

    // Read the SecurityResult word when it is actually sent:
    //  - after VNC Authentication in EVERY version, and
    //  - after ANY type (incl. None) in RFB 3.8+.
    // A failure reason string only accompanies the result in 3.8+; pre-3.8
    // servers send just the 4-byte result with no reason, so we must not try
    // to read one (that would block/desync).
    if did_vnc_auth || rfb_minor >= 8 {
        read_security_result(&mut plain, rfb_minor >= 8).await?;
    }

    Ok(plain)
}

/// Read the 4-byte SecurityResult; on failure, read the reason string only
/// when the negotiated version sends one (RFB 3.8+).
async fn read_security_result<S>(stream: &mut S, with_reason: bool) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    let result = stream.read_u32().await?;
    if result != 0 {
        if with_reason {
            let reason = read_reason_string(stream).await.unwrap_or_default();
            if reason.is_empty() {
                bail!("Authentication failed");
            } else {
                bail!("Authentication failed: {reason}");
            }
        } else {
            bail!("Authentication failed (wrong password?)");
        }
    }
    Ok(())
}

/// Return the supplied password or ask the UI for one via the event/command channels.
///
/// Blocks the current (background) thread until the UI responds or times out.
fn resolve_password(
    password: Option<&str>,
    event_tx: &Sender<VncEvent>,
    cmd_rx: &Receiver<VncCommand>,
) -> Result<String> {
    if let Some(pw) = password {
        if !pw.is_empty() {
            return Ok(pw.to_string());
        }
    }

    // Tell the UI we need a password.
    let _ = event_tx.send(VncEvent::NeedPassword);

    // Block-wait for the UI to provide one (or for a disconnect command).
    loop {
        match cmd_rx.recv_timeout(std::time::Duration::from_secs(120)) {
            Ok(VncCommand::ProvidePassword(pw)) => return Ok(pw),
            Ok(VncCommand::Disconnect) => bail!("Disconnected by user"),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                bail!("Timed out waiting for password");
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                bail!("UI channel closed");
            }
            Ok(_) => {} // ignore key/pointer/clipboard commands during auth
        }
    }
}

async fn read_reason_string<S>(stream: &mut S) -> Result<String>
where
    S: AsyncRead + Unpin,
{
    // Cap the server-controlled length: a hostile server could otherwise send
    // 0xFFFFFFFF and force a ~4 GB allocation before we read anything.
    const MAX_REASON: usize = 64 * 1024;
    let len = (stream.read_u32().await? as usize).min(MAX_REASON);
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// VNC DES challenge-response authentication.
async fn vnc_auth<S>(stream: &mut S, password: &str) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut challenge = [0u8; 16];
    stream.read_exact(&mut challenge).await?;

    let response = vnc_des_response(password, &challenge);
    stream.write_all(&response).await?;
    Ok(())
}

/// Encrypt the 16-byte challenge with VNC's quirky DES scheme.
/// VNC reverses the bit order of each key byte before using it.
fn vnc_des_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (i, &b) in password.as_bytes().iter().take(8).enumerate() {
        key[i] = b.reverse_bits();
    }

    let cipher = Des::new_from_slice(&key).expect("DES key size is always 8");
    let mut response = [0u8; 16];

    let mut block1 = GenericArray::from(<[u8; 8]>::try_from(&challenge[..8]).unwrap());
    let mut block2 = GenericArray::from(<[u8; 8]>::try_from(&challenge[8..]).unwrap());
    cipher.encrypt_block(&mut block1);
    cipher.encrypt_block(&mut block2);
    response[..8].copy_from_slice(&block1);
    response[8..].copy_from_slice(&block2);
    response
}
