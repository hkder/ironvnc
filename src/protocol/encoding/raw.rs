use crate::framebuffer::Framebuffer;
use crate::protocol::messages::PixelFormat;
use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt};

pub async fn decode<S>(
    stream: &mut S,
    fb: &mut Framebuffer,
    pf: &PixelFormat,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    let bpp = (pf.bits_per_pixel / 8) as usize;
    let total = w as usize * h as usize * bpp;
    let mut buf = vec![0u8; total];
    stream.read_exact(&mut buf).await?;

    let mut rgba = vec![0u8; w as usize * h as usize * 4];
    for i in 0..(w as usize * h as usize) {
        let pixel = read_pixel(&buf[i * bpp..], bpp, pf.big_endian);
        let (r, g, b) = pf.to_rgb(pixel);
        rgba[i * 4] = r;
        rgba[i * 4 + 1] = g;
        rgba[i * 4 + 2] = b;
        rgba[i * 4 + 3] = 255;
    }

    fb.blit_rgba(x as u32, y as u32, w as u32, h as u32, &rgba);
    Ok(())
}

#[inline]
pub fn read_pixel(buf: &[u8], bpp: usize, big_endian: bool) -> u32 {
    match (bpp, big_endian) {
        (4, true) => u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        (4, false) => u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
        // 3-byte CPIXEL (ZRLE/24bpp): the pixel's significant 3 bytes, sent in
        // the pixel format's byte order. Zero-extend the missing 4th byte.
        (3, true) => u32::from_be_bytes([0, buf[0], buf[1], buf[2]]),
        (3, false) => u32::from_le_bytes([buf[0], buf[1], buf[2], 0]),
        (2, true) => u16::from_be_bytes([buf[0], buf[1]]) as u32,
        (2, false) => u16::from_le_bytes([buf[0], buf[1]]) as u32,
        (1, _) => buf[0] as u32,
        _ => 0,
    }
}
