/// Hextile encoding — divides rectangles into 16×16 tiles with per-tile subtypes.
/// Very widely supported; many servers prefer it over raw.
use crate::framebuffer::Framebuffer;
use crate::protocol::encoding::raw::read_pixel;
use crate::protocol::messages::PixelFormat;
use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt};

// Subencode bitmask values
const RAW: u8 = 0x01;
const BG_SPECIFIED: u8 = 0x02;
const FG_SPECIFIED: u8 = 0x04;
const ANY_SUBRECTS: u8 = 0x08;
const SUBRECTS_COLOURED: u8 = 0x10;

pub async fn decode<S>(
    stream: &mut S,
    fb: &mut Framebuffer,
    pf: &PixelFormat,
    rect_x: u16,
    rect_y: u16,
    rect_w: u16,
    rect_h: u16,
) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    let bpp = (pf.bits_per_pixel / 8) as usize;
    // bg and fg persist across tiles within a rectangle.
    let mut bg_buf = [0u8; 4];
    let mut fg_buf = [0u8; 4];

    let mut ty = 0u16;
    while ty < rect_h {
        let tile_h = 16u16.min(rect_h - ty);
        let mut tx = 0u16;
        while tx < rect_w {
            let tile_w = 16u16.min(rect_w - tx);
            let n = tile_w as usize * tile_h as usize;

            let sub = stream.read_u8().await?;

            if sub & RAW != 0 {
                // Raw tile: bpp bytes per pixel, no background/subrect logic.
                let mut raw = vec![0u8; n * bpp];
                stream.read_exact(&mut raw).await?;
                let rgba = raw_to_rgba(&raw, bpp, pf, n);
                fb.blit_rgba(
                    rect_x as u32 + tx as u32,
                    rect_y as u32 + ty as u32,
                    tile_w as u32,
                    tile_h as u32,
                    &rgba,
                );
                tx += tile_w;
                continue;
            }

            if sub & BG_SPECIFIED != 0 {
                stream.read_exact(&mut bg_buf[..bpp]).await?;
            }
            if sub & FG_SPECIFIED != 0 {
                stream.read_exact(&mut fg_buf[..bpp]).await?;
            }

            // Fill tile with background colour.
            let (bgr, bgg, bgb) = pf.to_rgb(read_pixel(&bg_buf, bpp, pf.big_endian));
            let mut rgba = vec![255u8; n * 4];
            for i in 0..n {
                rgba[i * 4] = bgr;
                rgba[i * 4 + 1] = bgg;
                rgba[i * 4 + 2] = bgb;
            }

            if sub & ANY_SUBRECTS != 0 {
                let n_rects = stream.read_u8().await? as usize;
                let coloured = sub & SUBRECTS_COLOURED != 0;

                for _ in 0..n_rects {
                    let (sr, sg, sb) = if coloured {
                        let mut px_buf = [0u8; 4];
                        stream.read_exact(&mut px_buf[..bpp]).await?;
                        pf.to_rgb(read_pixel(&px_buf, bpp, pf.big_endian))
                    } else {
                        pf.to_rgb(read_pixel(&fg_buf, bpp, pf.big_endian))
                    };

                    let xy = stream.read_u8().await?;
                    let wh = stream.read_u8().await?;
                    let sx = (xy >> 4) as u32;
                    let sy = (xy & 0x0f) as u32;
                    let sw = ((wh >> 4) + 1) as u32;
                    let sh = ((wh & 0x0f) + 1) as u32;

                    for row in 0..sh {
                        for col in 0..sw {
                            let px = sx + col;
                            let py = sy + row;
                            if px < tile_w as u32 && py < tile_h as u32 {
                                let idx = (py * tile_w as u32 + px) as usize * 4;
                                if idx + 2 < rgba.len() {
                                    rgba[idx] = sr;
                                    rgba[idx + 1] = sg;
                                    rgba[idx + 2] = sb;
                                }
                            }
                        }
                    }
                }
            }

            fb.blit_rgba(
                rect_x as u32 + tx as u32,
                rect_y as u32 + ty as u32,
                tile_w as u32,
                tile_h as u32,
                &rgba,
            );
            tx += tile_w;
        }
        ty += tile_h;
    }
    Ok(())
}

fn raw_to_rgba(raw: &[u8], bpp: usize, pf: &PixelFormat, n: usize) -> Vec<u8> {
    let mut rgba = vec![255u8; n * 4];
    for i in 0..n {
        let s = i * bpp;
        if s + bpp > raw.len() {
            break;
        }
        let (r, g, b) = pf.to_rgb(read_pixel(&raw[s..], bpp, pf.big_endian));
        rgba[i * 4] = r;
        rgba[i * 4 + 1] = g;
        rgba[i * 4 + 2] = b;
    }
    rgba
}
