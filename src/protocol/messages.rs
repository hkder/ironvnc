/// RFB pixel format as negotiated during ServerInit.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    #[inline]
    pub fn to_rgb(&self, pixel: u32) -> (u8, u8, u8) {
        let r = ((pixel >> self.red_shift) & self.red_max as u32) * 255
            / self.red_max as u32;
        let g = ((pixel >> self.green_shift) & self.green_max as u32) * 255
            / self.green_max as u32;
        let b = ((pixel >> self.blue_shift) & self.blue_max as u32) * 255
            / self.blue_max as u32;
        (r as u8, g as u8, b as u8)
    }
}

/// RFB encoding type constants (full spec set; some reserved for future encodings).
#[allow(dead_code)]
pub mod encoding {
    pub const RAW: i32 = 0;
    pub const COPY_RECT: i32 = 1;
    pub const RRE: i32 = 2;
    pub const HEXTILE: i32 = 5;
    pub const TIGHT: i32 = 7;
    pub const ZRLE: i32 = 16;
    pub const CURSOR: i32 = -239;
    pub const DESKTOP_SIZE: i32 = -223;
}

/// Client → Server message type bytes.
#[allow(dead_code)]
pub mod client_msg {
    pub const SET_PIXEL_FORMAT: u8 = 0;
    pub const SET_ENCODINGS: u8 = 2;
    pub const FB_UPDATE_REQUEST: u8 = 3;
    pub const KEY_EVENT: u8 = 4;
    pub const POINTER_EVENT: u8 = 5;
    pub const CLIENT_CUT_TEXT: u8 = 6;
}

/// Server → Client message type bytes.
pub mod server_msg {
    pub const FB_UPDATE: u8 = 0;
    pub const SET_COLOUR_MAP_ENTRIES: u8 = 1;
    pub const BELL: u8 = 2;
    pub const SERVER_CUT_TEXT: u8 = 3;
}

/// A single rectangle in a FramebufferUpdate (available for future use).
#[allow(dead_code)]
pub struct Rectangle {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub encoding: i32,
}
