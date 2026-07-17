//! Kobo Elipsa 2E framebuffer backend through a small FBInk C shim.

use crate::surface::{PixFmt, Surface};
use std::ffi::{CStr, c_char, c_int, c_void};
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

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KoboFbState {
    bpp: u8,
    grayscale: u8,
    reserved: u16,
    rotation: u32,
}

unsafe extern "C" {
    fn riddle_kobo_fb_open(
        info: *mut KoboFbInfo,
        error: *mut c_char,
        error_len: usize,
    ) -> *mut c_void;
    fn riddle_kobo_fb_capture_state(
        state: *mut KoboFbState,
        error: *mut c_char,
        error_len: usize,
    ) -> c_int;
    fn riddle_kobo_fb_restore_state(
        state: *const KoboFbState,
        error: *mut c_char,
        error_len: usize,
    ) -> c_int;
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

fn ffi_error(buffer: &[c_char], fallback: &str) -> io::Error {
    let message = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    io::Error::other(if message.is_empty() {
        fallback.to_string()
    } else {
        message
    })
}

/// Capture the exact native framebuffer mode before Nickel is stopped.
/// The stable text form is persisted by launch.sh for crash recovery.
pub fn capture_state() -> io::Result<String> {
    let mut state = KoboFbState::default();
    let mut error = [0 as c_char; 192];
    let rv = unsafe { riddle_kobo_fb_capture_state(&mut state, error.as_mut_ptr(), error.len()) };
    if rv < 0 {
        return Err(ffi_error(&error, "cannot capture Kobo framebuffer state"));
    }
    Ok(format!(
        "{} {} {}",
        state.bpp, state.grayscale, state.rotation
    ))
}

/// Restore a state previously returned by `capture_state`.
pub fn restore_state(spec: &[String]) -> io::Result<()> {
    if spec.len() != 3 {
        return Err(io::Error::other(
            "restore state needs: BPP GRAYSCALE ROTATION",
        ));
    }
    let state = KoboFbState {
        bpp: spec[0].parse().map_err(io::Error::other)?,
        grayscale: spec[1].parse().map_err(io::Error::other)?,
        reserved: 0,
        rotation: spec[2].parse().map_err(io::Error::other)?,
    };
    let mut error = [0 as c_char; 192];
    let rv = unsafe { riddle_kobo_fb_restore_state(&state, error.as_mut_ptr(), error.len()) };
    if rv < 0 {
        return Err(ffi_error(&error, "cannot restore Kobo framebuffer state"));
    }
    Ok(())
}

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
            return Err(ffi_error(&error, "FBInk backend failed"));
        }
        if info.buffer.is_null() || (info.bpp != 8 && info.bpp != 32) {
            unsafe { riddle_kobo_fb_close(ctx) };
            return Err(io::Error::other(format!(
                "FBInk exposed unsupported {}bpp buffer",
                info.bpp
            )));
        }
        let format = if info.bpp == 8 {
            PixFmt::Gray8
        } else {
            PixFmt::Rgb32
        };
        let surface = Surface::new(
            info.buffer,
            info.buffer_size,
            info.width as usize,
            info.height as usize,
            info.stride as usize,
            format,
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
