//! Kobo Elipsa 2E framebuffer backend through a small FBInk C shim.

use crate::surface::{PixFmt, Surface};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::io;

#[repr(C)]
#[derive(Clone, Copy)]
struct KoboFbInfo {
    buffer: *mut u8,
    buffer_size: usize,
    width: u32,
    height: u32,
    stride: u32,
    bpp: u32,
    dpi: u32,
    device_name: [c_char; 32],
    device_codename: [c_char; 32],
}

impl Default for KoboFbInfo {
    fn default() -> Self {
        Self {
            buffer: std::ptr::null_mut(),
            buffer_size: 0,
            width: 0,
            height: 0,
            stride: 0,
            bpp: 0,
            dpi: 0,
            device_name: [0; 32],
            device_codename: [0; 32],
        }
    }
}

unsafe extern "C" {
    fn riddle_kobo_fb_open(
        info: *mut KoboFbInfo,
        error: *mut c_char,
        error_len: usize,
    ) -> *mut c_void;
    fn riddle_kobo_fb_refresh(
        fb: *mut c_void,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        mode: c_int,
    ) -> c_int;
    fn riddle_kobo_fb_close(fb: *mut c_void);
}

const REFRESH_FAST: c_int = 0;
const REFRESH_BALANCED: c_int = 1;
const REFRESH_FULL: c_int = 2;

pub struct KoboDisplay {
    ctx: *mut c_void,
    info: KoboFbInfo,
}

impl KoboDisplay {
    pub fn open() -> io::Result<(Self, Surface)> {
        let mut info = KoboFbInfo::default();
        let mut error = [0 as c_char; 192];
        let ctx = unsafe { riddle_kobo_fb_open(&mut info, error.as_mut_ptr(), error.len()) };
        if ctx.is_null() {
            let message = unsafe { CStr::from_ptr(error.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            return Err(io::Error::other(if message.is_empty() {
                "FBInk backend failed".to_string()
            } else {
                message
            }));
        }
        if info.buffer.is_null() || info.bpp != 8 {
            unsafe { riddle_kobo_fb_close(ctx) };
            return Err(io::Error::other("FBInk did not expose a Gray8 buffer"));
        }
        let surface = Surface::new(
            info.buffer,
            info.buffer_size,
            info.width as usize,
            info.height as usize,
            info.stride as usize,
            PixFmt::Gray8,
        );
        let display = Self { ctx, info };
        eprintln!(
            "riddle: Kobo {} ({}) {}x{} {}dpi",
            display.device_name(),
            display.device_codename(),
            info.width,
            info.height,
            info.dpi
        );
        Ok((display, surface))
    }

    fn text(bytes: &[c_char]) -> &str {
        unsafe { CStr::from_ptr(bytes.as_ptr()) }
            .to_str()
            .unwrap_or("?")
    }

    pub fn device_name(&self) -> &str {
        Self::text(&self.info.device_name)
    }

    pub fn device_codename(&self) -> &str {
        Self::text(&self.info.device_codename)
    }

    pub fn refresh(&self, x: i32, y: i32, width: i32, height: i32, fast: bool) {
        if width <= 0 || height <= 0 {
            return;
        }
        let x = x.max(0) as u32;
        let y = y.max(0) as u32;
        let width = (width as u32).min(self.info.width.saturating_sub(x));
        let height = (height as u32).min(self.info.height.saturating_sub(y));
        let mode = if fast { REFRESH_FAST } else { REFRESH_BALANCED };
        let rv = unsafe { riddle_kobo_fb_refresh(self.ctx, x, y, width, height, mode) };
        if rv < 0 {
            eprintln!("riddle: FBInk partial refresh failed ({rv})");
        }
    }

    pub fn full_refresh(&self) {
        let rv = unsafe { riddle_kobo_fb_refresh(self.ctx, 0, 0, 0, 0, REFRESH_FULL) };
        if rv < 0 {
            eprintln!("riddle: FBInk full refresh failed ({rv})");
        }
    }
}

impl Drop for KoboDisplay {
    fn drop(&mut self) {
        unsafe { riddle_kobo_fb_close(self.ctx) };
        self.ctx = std::ptr::null_mut();
    }
}
