pub mod client;
pub mod encoding;
pub mod messages;
pub mod ra2;
pub mod security;

/// Object-safe alias for a bidirectional async stream. The security handshake
/// may replace the raw TCP stream with an AES-encrypted channel (RA2), so the
/// client works against this trait object after negotiation.
pub trait AsyncRw: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AsyncRw for T {}
