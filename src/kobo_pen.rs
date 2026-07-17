//! Kobo Elipsa 2E (Condor) stylus input.
//!
//! Condor reports fingers and the Kobo Stylus 2 through one multitouch evdev
//! stream. Pen slots carry ABS_MT_TOOL_TYPE=MT_TOOL_PEN and pressure in
//! ABS_MT_PRESSURE. The panel's Y axis is physically mirrored.

use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::RawFd;

use crate::fb::{SCREEN_H, SCREEN_W};

pub const MAX_PRESSURE: i32 = 4096;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const BTN_TOUCH: u16 = 330;
const BTN_STYLUS: u16 = 331;
const BTN_STYLUS2: u16 = 332;
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TOOL_TYPE: u16 = 55;
const ABS_MT_TRACKING_ID: u16 = 57;
const ABS_MT_PRESSURE: u16 = 58;
const MT_TOOL_PEN: i32 = 1;
const MAX_SLOTS: usize = 16;
const EVIOCGRAB: libc::c_ulong = 0x40044590;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Eraser,
}

#[derive(Debug, Clone, Copy)]
pub struct PenSample {
    pub x: i32,
    pub y: i32,
    pub pressure: i32,
    pub tool: Tool,
    pub touching: bool,
    pub proximity: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[derive(Clone, Copy, Debug)]
struct Axis {
    min: i32,
    max: i32,
}

impl Axis {
    fn scale(self, value: i32, pixels: i32, mirrored: bool) -> i32 {
        let span = (self.max - self.min).max(1);
        let raw = (value - self.min).clamp(0, span);
        let raw = if mirrored { span - raw } else { raw };
        raw * (pixels - 1) / span
    }
}

#[derive(Clone, Copy, Debug)]
struct Axes {
    x: Axis,
    y: Axis,
    pressure: Axis,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    active: bool,
    x: i32,
    y: i32,
    pressure: i32,
    tool_type: i32,
    dirty: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            active: false,
            x: 0,
            y: 0,
            pressure: 0,
            tool_type: 0,
            dirty: false,
        }
    }
}

struct Parser {
    axes: Axes,
    slots: [Slot; MAX_SLOTS],
    current: usize,
    btn_touch: bool,
    eraser: bool,
    second_button: bool,
    quit_requested: bool,
    pen_was_present: bool,
    last_x: i32,
    last_y: i32,
}

impl Parser {
    fn new(axes: Axes) -> Self {
        Self {
            axes,
            slots: [Slot::default(); MAX_SLOTS],
            current: 0,
            btn_touch: false,
            eraser: false,
            second_button: false,
            quit_requested: false,
            pen_was_present: false,
            last_x: 0,
            last_y: 0,
        }
    }

    fn feed(&mut self, kind: u16, code: u16, value: i32, out: &mut Vec<PenSample>) {
        match (kind, code) {
            (EV_ABS, ABS_MT_SLOT) => self.current = (value.max(0) as usize).min(MAX_SLOTS - 1),
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let slot = &mut self.slots[self.current];
                slot.active = value >= 0;
                if !slot.active {
                    slot.pressure = 0;
                }
                slot.dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_X) => {
                self.slots[self.current].x = value;
                self.slots[self.current].dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                self.slots[self.current].y = value;
                self.slots[self.current].dirty = true;
            }
            (EV_ABS, ABS_MT_TOOL_TYPE) => {
                self.slots[self.current].tool_type = value;
                self.slots[self.current].dirty = true;
            }
            (EV_ABS, ABS_MT_PRESSURE) => {
                self.slots[self.current].pressure = value;
                self.slots[self.current].dirty = true;
            }
            (EV_KEY, BTN_TOUCH) => self.btn_touch = value != 0,
            (EV_KEY, BTN_STYLUS) => self.eraser = value != 0,
            (EV_KEY, BTN_STYLUS2) => self.second_button = value != 0,
            (EV_SYN, SYN_REPORT) => self.finish_frame(out),
            _ => {}
        }
    }

    fn finish_frame(&mut self, out: &mut Vec<PenSample>) {
        if self.eraser && self.second_button {
            self.quit_requested = true;
        }
        let pen_index = self
            .slots
            .iter()
            .position(|slot| slot.active && slot.tool_type == MT_TOOL_PEN);
        if let Some(index) = pen_index {
            let slot = &mut self.slots[index];
            let x = self.axes.x.scale(slot.x, SCREEN_W as i32, false);
            let y = self.axes.y.scale(slot.y, SCREEN_H as i32, true);
            let pressure = self
                .axes
                .pressure
                .scale(slot.pressure, MAX_PRESSURE + 1, false)
                .clamp(0, MAX_PRESSURE);
            let touching = pressure > 0 || self.btn_touch;
            if slot.dirty || !self.pen_was_present {
                out.push(PenSample {
                    x,
                    y,
                    pressure,
                    tool: if self.eraser { Tool::Eraser } else { Tool::Pen },
                    touching,
                    proximity: true,
                });
            }
            slot.dirty = false;
            self.pen_was_present = true;
            self.last_x = x;
            self.last_y = y;
        } else if self.pen_was_present {
            out.push(PenSample {
                x: self.last_x,
                y: self.last_y,
                pressure: 0,
                tool: if self.eraser { Tool::Eraser } else { Tool::Pen },
                touching: false,
                proximity: false,
            });
            self.pen_was_present = false;
        }
    }
}

pub struct PenDevice {
    fd: RawFd,
    parser: Parser,
}

impl PenDevice {
    pub fn open() -> io::Result<Self> {
        let (path, axes) = find_stylus_device()?;
        let cpath = std::ffi::CString::new(path.clone()).unwrap();
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let grab = unsafe { libc::ioctl(fd, EVIOCGRAB, 1i32) };
        if grab != 0 {
            let error = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(io::Error::new(
                error.kind(),
                format!("EVIOCGRAB failed for {path}: {error}"),
            ));
        }
        eprintln!(
            "riddle: Kobo stylus {path}, x={}..{}, y={}..{}, pressure={}..{}",
            axes.x.min, axes.x.max, axes.y.min, axes.y.max, axes.pressure.min, axes.pressure.max
        );
        Ok(Self {
            fd,
            parser: Parser::new(axes),
        })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    pub fn drain(&mut self) -> Vec<PenSample> {
        let mut out = Vec::new();
        let mut events: [InputEvent; 64] = unsafe { zeroed() };
        loop {
            let bytes = unsafe {
                libc::read(
                    self.fd,
                    events.as_mut_ptr().cast::<libc::c_void>(),
                    size_of::<InputEvent>() * events.len(),
                )
            };
            if bytes <= 0 {
                break;
            }
            for event in events.iter().take(bytes as usize / size_of::<InputEvent>()) {
                self.parser
                    .feed(event.kind, event.code, event.value, &mut out);
            }
        }
        out
    }

    pub fn take_quit_requested(&mut self) -> bool {
        let requested = self.parser.quit_requested;
        self.parser.quit_requested = false;
        requested
    }
}

impl Drop for PenDevice {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.fd, EVIOCGRAB, 0i32);
            libc::close(self.fd);
        }
    }
}

fn find_stylus_device() -> io::Result<(String, Axes)> {
    for index in 0..32 {
        let path = format!("/dev/input/event{index}");
        let Ok(cpath) = std::ffi::CString::new(path.clone()) else {
            continue;
        };
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        if fd < 0 {
            continue;
        }
        let result = (|| {
            // Requiring TOOL_TYPE prevents selecting a finger-only touch panel
            // which happens to expose a pressure axis.
            let _tool_type = query_axis(fd, ABS_MT_TOOL_TYPE)?;
            Some(Axes {
                x: query_axis(fd, ABS_MT_POSITION_X)?,
                y: query_axis(fd, ABS_MT_POSITION_Y)?,
                pressure: query_axis(fd, ABS_MT_PRESSURE)?,
            })
        })();
        unsafe { libc::close(fd) };
        if let Some(axes) = result {
            return Ok((path, axes));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no MT stylus device with position and pressure axes",
    ))
}

fn query_axis(fd: RawFd, axis: u16) -> Option<Axis> {
    // _IOR('E', 0x40 + axis, struct input_absinfo)
    let request = ((2u64 << 30)
        | ((size_of::<InputAbsInfo>() as u64) << 16)
        | ((b'E' as u64) << 8)
        | (0x40 + axis as u64)) as libc::c_ulong;
    let mut info = InputAbsInfo::default();
    let rv = unsafe { libc::ioctl(fd, request, &mut info) };
    (rv == 0 && info.maximum > info.minimum).then_some(Axis {
        min: info.minimum,
        max: info.maximum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> Parser {
        Parser::new(Axes {
            x: Axis { min: 0, max: 1000 },
            y: Axis { min: 0, max: 2000 },
            pressure: Axis { min: 0, max: 4095 },
        })
    }

    fn frame(parser: &mut Parser, events: &[(u16, u16, i32)]) -> Vec<PenSample> {
        let mut out = Vec::new();
        for &(kind, code, value) in events {
            parser.feed(kind, code, value, &mut out);
        }
        parser.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        out
    }

    #[test]
    fn ignores_fingers_and_maps_pen_with_mirrored_y() {
        let mut p = parser();
        let finger = frame(
            &mut p,
            &[
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 10),
                (EV_ABS, ABS_MT_TOOL_TYPE, 0),
                (EV_ABS, ABS_MT_POSITION_X, 500),
                (EV_ABS, ABS_MT_POSITION_Y, 500),
                (EV_ABS, ABS_MT_PRESSURE, 1000),
            ],
        );
        assert!(finger.is_empty());

        let pen = frame(
            &mut p,
            &[
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, 11),
                (EV_ABS, ABS_MT_TOOL_TYPE, MT_TOOL_PEN),
                (EV_ABS, ABS_MT_POSITION_X, 500),
                (EV_ABS, ABS_MT_POSITION_Y, 500),
                (EV_ABS, ABS_MT_PRESSURE, 2048),
            ],
        );
        assert_eq!(pen.len(), 1);
        assert!((pen[0].x - SCREEN_W as i32 / 2).abs() <= 1);
        assert!(pen[0].y > SCREEN_H as i32 / 2, "Y should be mirrored");
        assert!(pen[0].touching);
        assert_eq!(pen[0].tool, Tool::Pen);
    }

    #[test]
    fn side_button_selects_eraser_and_release_is_emitted() {
        let mut p = parser();
        let down = frame(
            &mut p,
            &[
                (EV_KEY, BTN_STYLUS, 1),
                (EV_ABS, ABS_MT_SLOT, 2),
                (EV_ABS, ABS_MT_TRACKING_ID, 12),
                (EV_ABS, ABS_MT_TOOL_TYPE, MT_TOOL_PEN),
                (EV_ABS, ABS_MT_POSITION_X, 100),
                (EV_ABS, ABS_MT_POSITION_Y, 100),
                (EV_ABS, ABS_MT_PRESSURE, 3000),
            ],
        );
        assert_eq!(down[0].tool, Tool::Eraser);
        let up = frame(
            &mut p,
            &[(EV_ABS, ABS_MT_SLOT, 2), (EV_ABS, ABS_MT_TRACKING_ID, -1)],
        );
        assert_eq!(up.len(), 1);
        assert!(!up[0].touching);
        assert!(!up[0].proximity);
    }

    #[test]
    fn both_stylus_buttons_request_safe_exit() {
        let mut p = parser();
        let mut out = Vec::new();
        p.feed(EV_KEY, BTN_STYLUS, 1, &mut out);
        p.feed(EV_KEY, BTN_STYLUS2, 1, &mut out);
        p.feed(EV_SYN, SYN_REPORT, 0, &mut out);
        assert!(p.quit_requested);
    }
}
