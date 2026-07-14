# IronVNC

A free, open-source **VNC (RFB) viewer written in Rust** — a lightweight alternative to RealVNC / TightVNC / UltraVNC with a clean egui interface, modern encodings, RealVNC-compatible encryption, and a built-in SFTP file browser.

> Iron oxidizes into rust. 🦀

## Features

- **Broad server compatibility** — RFB 3.3 / 3.7 / 3.8, with automatic version negotiation (never downgrades a modern server to legacy auth).
- **Encodings:** Raw, CopyRect, Hextile, ZRLE, Tight (JPEG/Fill/Palette/Gradient), plus the DesktopSize and Cursor pseudo-encodings.
- **Authentication:**
  - None
  - VNC Authentication (DES challenge/response)
  - **RA2 / RSA-AES** (types 5, 6, 129, 130) — connects to modern **RealVNC** servers, including AES‑256, with full-session or handshake-only encryption.
- **Session manager** — saved connections with credentials; single click opens each in its own window. Connections auto-save on connect.
- **MobaXterm-style SFTP panel** — browse the remote host over SSH, drag-and-drop to upload, download with a click. Runs alongside the VNC session, independent of the VNC vendor.
- **Quality-of-life:** smooth scale-to-fit rendering, clipboard sync (both directions), scroll-wheel support, a Ctrl+Alt+Del button, desktop-name window titles, and partial-framebuffer updates for responsiveness at 4K.

## Install / Build

Requires a [Rust toolchain](https://rustup.rs/).

```bash
git clone https://github.com/hkder/ironvnc
cd ironvnc
cargo build --release
# binary at target/release/ironvnc
```

## Usage

Launch with no arguments for the session manager:

```bash
ironvnc
```

Or connect directly:

```bash
ironvnc --host 192.168.1.50            # defaults to port 5900
ironvnc --host 192.168.1.50 --port 5901
ironvnc --session "My Desktop"          # open a saved session by name
```

If the server requires a password, IronVNC prompts for it in a popup and (on success) saves the session so you don't have to re-enter it. Saved sessions live in `sessions.json` **next to the executable** — the path is shown at the bottom of the session manager.

### Headless self-test

```bash
ironvnc --test -H <host> -P <password>            # connect, verify a framebuffer decodes, exit
ironvnc --sftp-test -H <host> --user <u> -P <pw>  # verify SSH/SFTP connectivity
```

## Security notes

- `sessions.json` stores passwords in plaintext and is **gitignored** — do not commit it.
- The RA2 host key is currently accepted without pinning (convenient for trusted networks); certificate/known-hosts verification is a planned improvement.
- VeNCrypt (TLS) and Apple ARD authentication are not yet implemented.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
