/// RGBA pixel buffer representing the remote desktop.
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA bytes (4 bytes per pixel).
    pub pixels: Vec<u8>,
}

impl Framebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0u8; (width * height * 4) as usize],
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0u8; (width * height * 4) as usize];
    }

    #[allow(dead_code)]
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = ((y * self.width + x) * 4) as usize;
        self.pixels[idx] = r;
        self.pixels[idx + 1] = g;
        self.pixels[idx + 2] = b;
        self.pixels[idx + 3] = 255;
    }

    /// Copy raw RGBA data into a sub-rectangle.
    pub fn blit_rgba(&mut self, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        for row in 0..h {
            let src_start = (row * w * 4) as usize;
            let src_end = src_start + (w * 4) as usize;
            let dst_y = y + row;
            if dst_y >= self.height {
                break;
            }
            let dst_start = ((dst_y * self.width + x) * 4) as usize;
            let dst_end = dst_start + (w * 4) as usize;
            if dst_end > self.pixels.len() || src_end > data.len() {
                break;
            }
            self.pixels[dst_start..dst_end].copy_from_slice(&data[src_start..src_end]);
        }
    }

    /// Copy a rectangle within the framebuffer (CopyRect encoding).
    pub fn copy_rect(&mut self, dst_x: u32, dst_y: u32, src_x: u32, src_y: u32, w: u32, h: u32) {
        // Collect source rows first to handle overlapping regions.
        let row_bytes = (w * 4) as usize;
        let mut rows: Vec<Vec<u8>> = Vec::with_capacity(h as usize);
        for row in 0..h {
            let sy = src_y + row;
            if sy >= self.height {
                break;
            }
            let start = ((sy * self.width + src_x) * 4) as usize;
            rows.push(self.pixels[start..start + row_bytes].to_vec());
        }
        for (row, data) in rows.iter().enumerate() {
            let dy = dst_y + row as u32;
            if dy >= self.height {
                break;
            }
            let start = ((dy * self.width + dst_x) * 4) as usize;
            if start + row_bytes <= self.pixels.len() {
                self.pixels[start..start + row_bytes].copy_from_slice(data);
            }
        }
    }

    /// Extract a sub-rectangle's RGBA bytes (clamped to bounds), tightly packed
    /// as `w*h*4` bytes. Returns the clamped (w, h) alongside the data.
    pub fn sub_rgba(&self, x: u32, y: u32, w: u32, h: u32) -> (u32, u32, Vec<u8>) {
        let x = x.min(self.width);
        let y = y.min(self.height);
        let w = w.min(self.width - x);
        let h = h.min(self.height - y);
        let mut out = vec![0u8; (w * h * 4) as usize];
        let row_bytes = (w * 4) as usize;
        for row in 0..h {
            let src = (((y + row) * self.width + x) * 4) as usize;
            let dst = (row * w * 4) as usize;
            out[dst..dst + row_bytes].copy_from_slice(&self.pixels[src..src + row_bytes]);
        }
        (w, h, out)
    }
}
