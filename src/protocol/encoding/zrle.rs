/// ZRLE encoding: shared stateful zlib stream, split into 64x64 tiles with RLE sub-encoding.
use crate::framebuffer::Framebuffer;
use crate::protocol::encoding::raw::read_pixel;
use crate::protocol::messages::PixelFormat;
use anyhow::{bail, Result};
use flate2::{Decompress, FlushDecompress};
use tokio::io::{AsyncRead, AsyncReadExt};

pub async fn decode<S>(
    stream: &mut S,
    fb: &mut Framebuffer,
    pf: &PixelFormat,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    decomp: &mut Decompress,
) -> Result<()>
where
    S: AsyncRead + Unpin,
{
    let data_len = stream.read_u32().await? as usize;
    let mut compressed = vec![0u8; data_len];
    stream.read_exact(&mut compressed).await?;

    // Upper bound on decompressed output: raw pixels + per-tile headers.
    let max_out = (w as usize * h as usize * (pf.bits_per_pixel / 8) as usize) + 4096;
    let mut raw = Vec::with_capacity(max_out);

    // ZRLE servers use Z_SYNC_FLUSH so all bytes for this rectangle are in `compressed`.
    decomp.decompress_vec(&compressed, &mut raw, FlushDecompress::Sync)?;

    decode_tiles(&raw, fb, pf, x, y, w, h)?;
    Ok(())
}

fn decode_tiles(
    raw: &[u8],
    fb: &mut Framebuffer,
    pf: &PixelFormat,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> Result<()> {
    let bpp = (pf.bits_per_pixel / 8) as usize;
    let cpixel_bytes = if bpp == 4 { 3usize } else { bpp };
    let mut pos = 0usize;

    let mut ty = 0u16;
    while ty < h {
        let tile_h = 64u16.min(h - ty);
        let mut tx = 0u16;
        while tx < w {
            let tile_w = 64u16.min(w - tx);
            let n_pixels = tile_w as usize * tile_h as usize;

            if pos >= raw.len() {
                bail!("ZRLE data truncated");
            }
            let subtype = raw[pos];
            pos += 1;

            let pixels: Vec<(u8, u8, u8)> = match subtype {
                0 => {
                    // Raw
                    let need = n_pixels * cpixel_bytes;
                    if pos + need > raw.len() {
                        bail!("ZRLE raw data truncated");
                    }
                    let out = (0..n_pixels)
                        .map(|i| {
                            let p = read_pixel(
                                &raw[pos + i * cpixel_bytes..],
                                cpixel_bytes,
                                pf.big_endian,
                            );
                            pf.to_rgb(p)
                        })
                        .collect();
                    pos += need;
                    out
                }
                1 => {
                    // Solid colour
                    if pos + cpixel_bytes > raw.len() {
                        bail!("ZRLE solid truncated");
                    }
                    let p = read_pixel(&raw[pos..], cpixel_bytes, pf.big_endian);
                    pos += cpixel_bytes;
                    vec![pf.to_rgb(p); n_pixels]
                }
                2..=16 => {
                    // Packed palette
                    let palette_size = subtype as usize;
                    let need = palette_size * cpixel_bytes;
                    if pos + need > raw.len() {
                        bail!("ZRLE palette truncated");
                    }
                    let palette: Vec<(u8, u8, u8)> = (0..palette_size)
                        .map(|i| {
                            let p = read_pixel(
                                &raw[pos + i * cpixel_bytes..],
                                cpixel_bytes,
                                pf.big_endian,
                            );
                            pf.to_rgb(p)
                        })
                        .collect();
                    pos += need;

                    let bits_per_idx = if palette_size <= 2 {
                        1
                    } else if palette_size <= 4 {
                        2
                    } else {
                        4
                    };
                    let row_bytes = (tile_w as usize * bits_per_idx + 7) / 8;
                    let need2 = row_bytes * tile_h as usize;
                    if pos + need2 > raw.len() {
                        bail!("ZRLE packed palette data truncated");
                    }
                    let mut out = Vec::with_capacity(n_pixels);
                    for row in 0..tile_h as usize {
                        let row_data =
                            &raw[pos + row * row_bytes..pos + row * row_bytes + row_bytes];
                        let mut bit_pos = 0usize;
                        for _ in 0..tile_w as usize {
                            let byte_idx = bit_pos / 8;
                            let idx = if bits_per_idx == 1 {
                                (row_data[byte_idx] >> (7 - bit_pos % 8)) & 1
                            } else if bits_per_idx == 2 {
                                (row_data[byte_idx] >> (6 - (bit_pos % 4) * 2)) & 3
                            } else if bit_pos % 2 == 0 {
                                (row_data[byte_idx] >> 4) & 0xf
                            } else {
                                row_data[byte_idx] & 0xf
                            };
                            out.push(palette[idx as usize]);
                            bit_pos += bits_per_idx;
                        }
                    }
                    pos += need2;
                    out
                }
                128 => {
                    // Plain RLE
                    let mut out = Vec::with_capacity(n_pixels);
                    while out.len() < n_pixels {
                        if pos + cpixel_bytes > raw.len() {
                            bail!("ZRLE RLE pixel truncated");
                        }
                        let p = read_pixel(&raw[pos..], cpixel_bytes, pf.big_endian);
                        pos += cpixel_bytes;
                        let c = pf.to_rgb(p);
                        let mut run_len = 1usize;
                        loop {
                            if pos >= raw.len() {
                                bail!("ZRLE RLE run length truncated");
                            }
                            let b = raw[pos] as usize;
                            pos += 1;
                            run_len += b;
                            if b < 255 {
                                break;
                            }
                        }
                        for _ in 0..run_len {
                            out.push(c);
                        }
                    }
                    out
                }
                t if t >= 130 => {
                    // Palette RLE
                    let palette_size = (t - 128) as usize;
                    let need = palette_size * cpixel_bytes;
                    if pos + need > raw.len() {
                        bail!("ZRLE palette RLE header truncated");
                    }
                    let palette: Vec<(u8, u8, u8)> = (0..palette_size)
                        .map(|i| {
                            let p = read_pixel(
                                &raw[pos + i * cpixel_bytes..],
                                cpixel_bytes,
                                pf.big_endian,
                            );
                            pf.to_rgb(p)
                        })
                        .collect();
                    pos += need;
                    let mut out = Vec::with_capacity(n_pixels);
                    while out.len() < n_pixels {
                        if pos >= raw.len() {
                            bail!("ZRLE palette RLE idx truncated");
                        }
                        let idx_byte = raw[pos];
                        pos += 1;
                        let idx = (idx_byte & 0x7f) as usize;
                        if idx >= palette.len() {
                            bail!("ZRLE palette index out of range");
                        }
                        let c = palette[idx];
                        if idx_byte & 0x80 != 0 {
                            let mut run_len = 1usize;
                            loop {
                                if pos >= raw.len() {
                                    bail!("ZRLE palette RLE run truncated");
                                }
                                let b = raw[pos] as usize;
                                pos += 1;
                                run_len += b;
                                if b < 255 {
                                    break;
                                }
                            }
                            for _ in 0..run_len {
                                out.push(c);
                            }
                        } else {
                            out.push(c);
                        }
                    }
                    out
                }
                _ => bail!("Unknown ZRLE subtype {subtype}"),
            };

            let abs_x = x as u32 + tx as u32;
            let abs_y = y as u32 + ty as u32;
            let mut rgba = vec![0u8; n_pixels * 4];
            for (i, (r, g, b)) in pixels.iter().enumerate() {
                rgba[i * 4] = *r;
                rgba[i * 4 + 1] = *g;
                rgba[i * 4 + 2] = *b;
                rgba[i * 4 + 3] = 255;
            }
            fb.blit_rgba(abs_x, abs_y, tile_w as u32, tile_h as u32, &rgba);

            tx += tile_w;
        }
        ty += tile_h;
    }
    Ok(())
}
