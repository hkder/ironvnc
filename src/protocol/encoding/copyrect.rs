use crate::framebuffer::Framebuffer;
use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt};

pub async fn decode<S>(
    stream: &mut S,
    fb: &mut Framebuffer,
    dst_x: u16,
    dst_y: u16,
    w: u16,
    h: u16,
) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    let src_x = stream.read_u16().await?;
    let src_y = stream.read_u16().await?;
    fb.copy_rect(
        dst_x as u32,
        dst_y as u32,
        src_x as u32,
        src_y as u32,
        w as u32,
        h as u32,
    );
    Ok(())
}
