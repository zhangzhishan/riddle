//! Pure interaction model for the Paper Plane Mailbox.
//!
//! This module deliberately contains no framebuffer, stylus, or HTTP code.
//! The Kobo loop translates taps and network completions into these events,
//! which keeps accidental sends and error recovery host-testable.

use crate::fb::{SCREEN_H, SCREEN_W};

pub const ACTION_BAR_H: i32 = 180;
const AUTO_POLL_MS: u64 = 60_000;
const MAILBOX_FONT_TTF: &[u8] = include_bytes!("../fonts/mailbox/MaShanZheng-Regular.ttf");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    Upload,
    Poll,
    Refine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Compose,
    ConfirmSend,
    ConfirmRefine,
    ConfirmClear,
    Sending,
    Refining,
    CheckingInbox,
    Reply { id: u64, body: String },
    Refined,
    Error { message: String, retry: Retry },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Draw,
    Upload,
    Refine,
    Poll { after: u64 },
    ClearCanvas,
    DismissReply,
    DismissRefinement,
    AdoptRefinement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeTarget {
    Canvas,
    Send,
    Refine,
    Inbox,
    Clear,
    Outside,
}

pub struct MailboxState {
    pub screen: Screen,
    pub has_ink: bool,
    pub last_reply_id: u64,
    last_poll_ms: u64,
}

impl MailboxState {
    pub fn new(last_reply_id: u64, now_ms: u64) -> Self {
        Self {
            screen: Screen::Compose,
            has_ink: false,
            last_reply_id,
            last_poll_ms: now_ms,
        }
    }

    pub fn target(x: i32, y: i32) -> ComposeTarget {
        if x < 0 || y < 0 || x >= SCREEN_W as i32 || y >= SCREEN_H as i32 {
            return ComposeTarget::Outside;
        }
        let bar_y = SCREEN_H as i32 - ACTION_BAR_H;
        if y < bar_y {
            return ComposeTarget::Canvas;
        }
        let quarter = SCREEN_W as i32 / 4;
        if x < quarter {
            ComposeTarget::Send
        } else if x < quarter * 2 {
            ComposeTarget::Refine
        } else if x < quarter * 3 {
            ComposeTarget::Inbox
        } else {
            ComposeTarget::Clear
        }
    }

    pub fn note_ink(&mut self) {
        if matches!(self.screen, Screen::Compose) {
            self.has_ink = true;
        }
    }

    pub fn tap(&mut self, x: i32, y: i32, now_ms: u64) -> Action {
        match &self.screen {
            Screen::Compose => match Self::target(x, y) {
                ComposeTarget::Canvas => Action::Draw,
                ComposeTarget::Send if self.has_ink => {
                    self.screen = Screen::ConfirmSend;
                    Action::None
                }
                ComposeTarget::Refine if self.has_ink => {
                    self.screen = Screen::ConfirmRefine;
                    Action::None
                }
                ComposeTarget::Inbox => {
                    self.screen = Screen::CheckingInbox;
                    self.last_poll_ms = now_ms;
                    Action::Poll {
                        after: self.last_reply_id,
                    }
                }
                ComposeTarget::Clear if self.has_ink => {
                    self.screen = Screen::ConfirmClear;
                    Action::None
                }
                _ => Action::None,
            },
            Screen::ConfirmSend => {
                if confirm_target(x, y) {
                    self.screen = Screen::Sending;
                    Action::Upload
                } else {
                    self.screen = Screen::Compose;
                    Action::None
                }
            }
            Screen::ConfirmRefine => {
                if confirm_target(x, y) {
                    self.screen = Screen::Refining;
                    Action::Refine
                } else {
                    self.screen = Screen::Compose;
                    Action::None
                }
            }
            Screen::ConfirmClear => {
                if confirm_target(x, y) {
                    self.has_ink = false;
                    self.screen = Screen::Compose;
                    Action::ClearCanvas
                } else {
                    self.screen = Screen::Compose;
                    Action::None
                }
            }
            Screen::Reply { .. } => {
                self.screen = Screen::Compose;
                Action::DismissReply
            }
            Screen::Refined => {
                self.screen = Screen::Compose;
                if confirm_target(x, y) {
                    self.has_ink = true;
                    Action::AdoptRefinement
                } else {
                    Action::DismissRefinement
                }
            }
            Screen::Error { retry, .. } => {
                let retry = *retry;
                if confirm_target(x, y) {
                    match retry {
                        Retry::Upload => {
                            self.screen = Screen::Sending;
                            Action::Upload
                        }
                        Retry::Poll => {
                            self.screen = Screen::CheckingInbox;
                            self.last_poll_ms = now_ms;
                            Action::Poll {
                                after: self.last_reply_id,
                            }
                        }
                        Retry::Refine => {
                            self.screen = Screen::Refining;
                            Action::Refine
                        }
                    }
                } else {
                    self.screen = Screen::Compose;
                    Action::None
                }
            }
            Screen::Sending | Screen::Refining | Screen::CheckingInbox => Action::None,
        }
    }

    pub fn upload_finished(&mut self, result: Result<u64, String>) -> Action {
        if !matches!(self.screen, Screen::Sending) {
            return Action::None;
        }
        match result {
            Ok(_) => {
                self.has_ink = false;
                self.screen = Screen::Compose;
                Action::ClearCanvas
            }
            Err(message) => {
                self.screen = Screen::Error {
                    message,
                    retry: Retry::Upload,
                };
                Action::None
            }
        }
    }

    pub fn refine_finished(&mut self, result: Result<(), String>) -> Action {
        if !matches!(self.screen, Screen::Refining) {
            return Action::None;
        }
        match result {
            Ok(()) => self.screen = Screen::Refined,
            Err(message) => {
                self.screen = Screen::Error {
                    message,
                    retry: Retry::Refine,
                };
            }
        }
        Action::None
    }

    pub fn poll_finished(&mut self, result: Result<Option<(u64, String)>, String>) -> Action {
        if !matches!(self.screen, Screen::CheckingInbox) {
            return Action::None;
        }
        match result {
            Ok(Some((id, body))) => {
                self.last_reply_id = self.last_reply_id.max(id);
                self.screen = Screen::Reply { id, body };
            }
            Ok(None) => self.screen = Screen::Compose,
            Err(message) => {
                self.screen = Screen::Error {
                    message,
                    retry: Retry::Poll,
                }
            }
        }
        Action::None
    }

    /// Background polling is the only idle action. It can never upload ink.
    pub fn tick(&mut self, now_ms: u64) -> Action {
        if matches!(self.screen, Screen::Compose)
            && now_ms.saturating_sub(self.last_poll_ms) >= AUTO_POLL_MS
        {
            self.last_poll_ms = now_ms;
            self.screen = Screen::CheckingInbox;
            return Action::Poll {
                after: self.last_reply_id,
            };
        }
        Action::None
    }
}

fn confirm_target(x: i32, y: i32) -> bool {
    x >= SCREEN_W as i32 / 2
        && x < SCREEN_W as i32
        && y >= SCREEN_H as i32 * 2 / 3
        && y < SCREEN_H as i32
}

fn decode_png_luma(bytes: &[u8]) -> std::io::Result<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(std::io::Error::other)?;
    let source_info = reader.info();
    let pixel_count = u64::from(source_info.width) * u64::from(source_info.height);
    if source_info.width == 0
        || source_info.height == 0
        || source_info.width > 4096
        || source_info.height > 4096
        || pixel_count > 16 * 1024 * 1024
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "refined PNG dimensions are unsafe",
        ));
    }
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(std::io::Error::other)?;
    if info.bit_depth != png::BitDepth::Eight {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "refined PNG must decode to 8-bit pixels",
        ));
    }
    let pixels = &buffer[..info.buffer_size()];
    let mut luma = Vec::with_capacity(info.width as usize * info.height as usize);
    let blend = |value: u8, alpha: u8| -> u8 {
        let alpha = u32::from(alpha);
        ((u32::from(value) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
    };
    match info.color_type {
        png::ColorType::Grayscale => luma.extend_from_slice(pixels),
        png::ColorType::GrayscaleAlpha => {
            for pixel in pixels.chunks_exact(2) {
                luma.push(blend(pixel[0], pixel[1]));
            }
        }
        png::ColorType::Rgb => {
            for pixel in pixels.chunks_exact(3) {
                luma.push(
                    ((u32::from(pixel[0]) * 77
                        + u32::from(pixel[1]) * 150
                        + u32::from(pixel[2]) * 29
                        + 128)
                        >> 8) as u8,
                );
            }
        }
        png::ColorType::Rgba => {
            for pixel in pixels.chunks_exact(4) {
                let value = ((u32::from(pixel[0]) * 77
                    + u32::from(pixel[1]) * 150
                    + u32::from(pixel[2]) * 29
                    + 128)
                    >> 8) as u8;
                luma.push(blend(value, pixel[3]));
            }
        }
        png::ColorType::Indexed => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "refined PNG palette expansion failed",
            ));
        }
    }
    let expected = info.width as usize * info.height as usize;
    if luma.len() != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "refined PNG has an invalid pixel buffer",
        ));
    }
    Ok((info.width as usize, info.height as usize, luma))
}

fn write_canvas_png(
    surface: &crate::surface::Surface,
    path: &std::path::Path,
    canvas_height: usize,
) -> std::io::Result<()> {
    let canvas_height = canvas_height.min(surface.h);
    if surface.w == 0 || canvas_height == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "canvas has zero dimensions",
        ));
    }
    let factor = surface.w.max(canvas_height).div_ceil(1024).max(1);
    let width = surface.w.div_ceil(factor);
    let height = canvas_height.div_ceil(factor);
    let mut gray = vec![255u8; width * height];
    for output_y in 0..height {
        for output_x in 0..width {
            let source_x0 = output_x * factor;
            let source_y0 = output_y * factor;
            let source_x1 = (source_x0 + factor).min(surface.w);
            let source_y1 = (source_y0 + factor).min(canvas_height);
            let mut sum = 0u32;
            let mut count = 0u32;
            for source_y in source_y0..source_y1 {
                for source_x in source_x0..source_x1 {
                    sum += u32::from(surface.luma(source_x as i32, source_y as i32));
                    count += 1;
                }
            }
            gray[output_y * width + output_x] = (sum / count.max(1)) as u8;
        }
    }

    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder
        .write_header()
        .map_err(std::io::Error::other)?
        .write_image_data(&gray)
        .map_err(std::io::Error::other)
}

#[cfg(all(feature = "kobo", target_os = "linux"))]
pub fn run() -> std::io::Result<()> {
    use crate::fb::BBox;
    use crate::ink::Ink;
    use crate::mailbox_client::{MailboxClient, Reply as MailReply};
    use crate::pen::{PenDevice, Tool, MAX_PRESSURE};
    use crate::surface::{Surface, BLACK, WHITE};
    use ab_glyph::FontRef;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    enum NetResult {
        Upload(Result<u64, String>),
        Refine(Result<(), String>),
        Poll(Result<Option<MailReply>, String>),
    }

    fn outbox_dir() -> PathBuf {
        std::env::var_os("MAILBOX_OUTBOX_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/mnt/onboard/.adds/riddle-kobo/outbox"))
    }

    fn read_last_reply(dir: &Path) -> u64 {
        std::fs::read_to_string(dir.join("last-reply-id"))
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0)
    }

    fn write_last_reply(dir: &Path, id: u64) {
        let _ = std::fs::write(dir.join("last-reply-id"), format!("{id}\n"));
    }

    fn start_upload(tx: mpsc::Sender<NetResult>, page: PathBuf) {
        std::thread::spawn(move || {
            let result = MailboxClient::from_env()
                .and_then(|client| client.send_png(page))
                .map_err(|error| error.to_string());
            let _ = tx.send(NetResult::Upload(result));
        });
    }

    fn start_refine(tx: mpsc::Sender<NetResult>, page: PathBuf, refined: PathBuf) {
        std::thread::spawn(move || {
            let result = MailboxClient::from_env()
                .and_then(|client| client.refine_png(page, refined))
                .map_err(|error| error.to_string());
            let _ = tx.send(NetResult::Refine(result));
        });
    }

    fn start_poll(tx: mpsc::Sender<NetResult>, after: u64) {
        std::thread::spawn(move || {
            let result = MailboxClient::from_env()
                .and_then(|client| client.poll_reply(after))
                .map_err(|error| error.to_string());
            let _ = tx.send(NetResult::Poll(result));
        });
    }

    fn draw_text(
        surf: &mut Surface,
        font: &FontRef,
        text: &str,
        px: f32,
        center_x: usize,
        y: usize,
    ) {
        let line = crate::script::rasterize_line(font, text, px);
        let x0 = center_x.saturating_sub(line.width / 2);
        for row in 0..line.height {
            for col in 0..line.width {
                if line.mask[row * line.width + col] {
                    surf.put_px((x0 + col) as i32, (y + row) as i32, BLACK);
                }
            }
        }
    }

    fn render_compose_shell(surf: &mut Surface, font: &FontRef) {
        surf.fill_rect(0, 0, SCREEN_W, SCREEN_H, WHITE);
        draw_text(surf, font, "Ian 的纸飞机信箱", 64.0, SCREEN_W / 2, 24);
        let y = SCREEN_H - ACTION_BAR_H as usize;
        surf.fill_rect(0, y, SCREEN_W, ACTION_BAR_H as usize, WHITE);
    }

    fn render_controls(surf: &mut Surface, font: &FontRef, state: &MailboxState) {
        let y = SCREEN_H - ACTION_BAR_H as usize;
        surf.fill_rect(0, y, SCREEN_W, ACTION_BAR_H as usize, WHITE);
        surf.fill_rect(0, y, SCREEN_W, 3, BLACK);
        match &state.screen {
            Screen::Compose => {
                let quarter = SCREEN_W / 4;
                for divider in 1..4 {
                    surf.fill_rect(quarter * divider, y, 2, ACTION_BAR_H as usize, BLACK);
                }
                draw_text(surf, font, "发送", 48.0, quarter / 2, y + 56);
                draw_text(surf, font, "AI润色", 48.0, quarter + quarter / 2, y + 56);
                draw_text(surf, font, "收信", 48.0, quarter * 2 + quarter / 2, y + 56);
                draw_text(surf, font, "清空", 48.0, quarter * 3 + quarter / 2, y + 56);
            }
            Screen::ConfirmSend => {
                surf.fill_rect(SCREEN_W / 2, y, 2, ACTION_BAR_H as usize, BLACK);
                draw_text(surf, font, "取消", 52.0, SCREEN_W / 4, y + 50);
                draw_text(surf, font, "现在发送", 52.0, SCREEN_W * 3 / 4, y + 50);
            }
            Screen::ConfirmRefine => {
                surf.fill_rect(SCREEN_W / 2, y, 2, ACTION_BAR_H as usize, BLACK);
                draw_text(surf, font, "取消", 52.0, SCREEN_W / 4, y + 50);
                draw_text(surf, font, "开始润色", 52.0, SCREEN_W * 3 / 4, y + 50);
            }
            Screen::ConfirmClear => {
                surf.fill_rect(SCREEN_W / 2, y, 2, ACTION_BAR_H as usize, BLACK);
                draw_text(surf, font, "取消", 52.0, SCREEN_W / 4, y + 50);
                draw_text(surf, font, "清空画纸", 52.0, SCREEN_W * 3 / 4, y + 50);
            }
            Screen::Sending => draw_text(surf, font, "正在折纸飞机……", 52.0, SCREEN_W / 2, y + 50),
            Screen::Refining => draw_text(
                surf,
                font,
                "GPT 正在认真润色，大约要一两分钟……",
                38.0,
                SCREEN_W / 2,
                y + 58,
            ),
            Screen::CheckingInbox => draw_text(
                surf,
                font,
                "正在看看有没有回信……",
                52.0,
                SCREEN_W / 2,
                y + 50,
            ),
            Screen::Reply { .. } => {
                draw_text(surf, font, "点一下回到画纸", 48.0, SCREEN_W / 2, y + 56)
            }
            Screen::Refined => {
                surf.fill_rect(SCREEN_W / 2, y, 2, ACTION_BAR_H as usize, BLACK);
                draw_text(surf, font, "回到原画", 48.0, SCREEN_W / 4, y + 56);
                draw_text(surf, font, "在新图上继续画", 44.0, SCREEN_W * 3 / 4, y + 58);
            }
            Screen::Error { message, .. } => {
                surf.fill_rect(SCREEN_W / 2, y, 2, ACTION_BAR_H as usize, BLACK);
                let short: String = message.chars().take(34).collect();
                draw_text(surf, font, "取消", 46.0, SCREEN_W / 4, y + 70);
                draw_text(surf, font, "再试一次", 46.0, SCREEN_W * 3 / 4, y + 70);
                draw_text(surf, font, &short, 30.0, SCREEN_W / 2, y + 16);
            }
        }
    }

    fn render_reply(
        surf: &mut Surface,
        display: &crate::display::Display,
        font: &FontRef,
        body: &str,
    ) {
        surf.fill_rect(0, 0, SCREEN_W, SCREEN_H, WHITE);
        draw_text(surf, font, "纸飞机飞回来了", 58.0, SCREEN_W / 2, 30);
        display.update_all(SCREEN_W, SCREEN_H);
        let lines = wrap_reply(font, body, 72.0, (SCREEN_W - 180) as f32);
        let mut y = 170i32;
        for text in lines.into_iter().take(12) {
            let mut raster = crate::script::rasterize_line(font, &text, 72.0);
            crate::script::thin(&mut raster);
            let x0 = (SCREEN_W as i32 - raster.width as i32) / 2;
            let strokes = crate::script::trace(&raster);
            let mut dirty = BBox::empty();
            for (index, stroke) in strokes.iter().enumerate() {
                let mut previous = None;
                for &(sx, sy) in stroke {
                    let (x, yy) = (x0 + sx, y + sy);
                    if let Some((px, py)) = previous {
                        surf.brush_line(px, py, x, yy, 2, BLACK);
                    } else {
                        surf.stamp(x, yy, 2, BLACK);
                    }
                    dirty.add(x, yy, 4);
                    previous = Some((x, yy));
                }
                if index % 18 == 17 && !dirty.is_empty() {
                    let (x, yy, w, h) = dirty.rect();
                    display.update(x, yy, w, h, true);
                    dirty = BBox::empty();
                    std::thread::sleep(Duration::from_millis(16));
                }
            }
            if !dirty.is_empty() {
                let (x, yy, w, h) = dirty.rect();
                display.update(x, yy, w, h, true);
            }
            y += 96;
            if y >= SCREEN_H as i32 - ACTION_BAR_H - 100 {
                break;
            }
        }
    }

    fn render_refined_png(surf: &mut Surface, path: &Path) -> std::io::Result<()> {
        let bytes = std::fs::read(path)?;
        let (source_w, source_h, pixels) = decode_png_luma(&bytes)?;
        if source_w == 0 || source_h == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "refined PNG has zero dimensions",
            ));
        }
        let canvas_h = SCREEN_H - ACTION_BAR_H as usize;
        let max_w = SCREEN_W.saturating_sub(40).max(1);
        let max_h = canvas_h.saturating_sub(40).max(1);
        let (target_w, target_h) =
            if max_w.saturating_mul(source_h) <= max_h.saturating_mul(source_w) {
                (max_w, (source_h.saturating_mul(max_w) / source_w).max(1))
            } else {
                ((source_w.saturating_mul(max_h) / source_h).max(1), max_h)
            };
        let x0 = (SCREEN_W - target_w) / 2;
        let y0 = (canvas_h - target_h) / 2;
        surf.fill_rect(0, 0, SCREEN_W, canvas_h, WHITE);
        for y in 0..target_h {
            let source_y = y.saturating_mul(source_h) / target_h;
            for x in 0..target_w {
                let source_x = x.saturating_mul(source_w) / target_w;
                let value = pixels[source_y * source_w + source_x];
                let red = (u16::from(value) * 31 / 255) << 11;
                let green = (u16::from(value) * 63 / 255) << 5;
                let blue = u16::from(value) * 31 / 255;
                surf.put_px((x0 + x) as i32, (y0 + y) as i32, red | green | blue);
            }
        }
        Ok(())
    }

    fn wrap_reply(font: &FontRef, text: &str, px: f32, max_width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut current = String::new();
        for ch in text.chars() {
            if ch == '\n' {
                lines.push(std::mem::take(&mut current));
                continue;
            }
            let mut candidate = current.clone();
            candidate.push(ch);
            if !current.is_empty() && crate::script::measure(font, &candidate, px) > max_width {
                lines.push(std::mem::take(&mut current));
            }
            current.push(ch);
        }
        if !current.is_empty() || lines.is_empty() {
            lines.push(current);
        }
        lines
    }

    let font = FontRef::try_from_slice(MAILBOX_FONT_TTF).map_err(std::io::Error::other)?;
    let (display, mut surface) = crate::display::Display::open()?;
    let mut pen = PenDevice::open()?;
    let outbox = outbox_dir();
    std::fs::create_dir_all(&outbox)?;
    let page_path = outbox.join("pending.png");
    let refined_path = outbox.join("refined.png");
    let started = Instant::now();
    let mut state = MailboxState::new(read_last_reply(&outbox), 0);
    let mut ink = Ink::new();
    let mut pen_down = false;
    let mut press_target = ComposeTarget::Outside;
    let mut dirty = BBox::empty();
    let mut last_flush = Instant::now();
    let mut compose_snapshot: Option<Vec<u8>> = None;
    let mut base_snapshot: Option<Vec<u8>> = None;
    let (net_tx, net_rx) = mpsc::channel();
    let stopping = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stopping))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stopping))?;

    render_compose_shell(&mut surface, &font);
    render_controls(&mut surface, &font, &state);
    display.full_refresh(SCREEN_W, SCREEN_H);

    let apply_action = |action: Action,
                        state: &mut MailboxState,
                        ink: &mut Ink,
                        surface: &mut Surface,
                        display: &crate::display::Display,
                        snapshot: &mut Option<Vec<u8>>,
                        base: &mut Option<Vec<u8>>| {
        match action {
            Action::Upload => {
                let export = if base.is_some() {
                    write_canvas_png(surface, &page_path, SCREEN_H - ACTION_BAR_H as usize)
                } else {
                    ink.to_png(surface, page_path.to_str().unwrap())
                };
                if let Err(error) = export {
                    state.upload_finished(Err(error.to_string()));
                } else {
                    start_upload(net_tx.clone(), page_path.clone());
                }
                render_controls(surface, &font, state);
                display.update(
                    0,
                    SCREEN_H as i32 - ACTION_BAR_H,
                    SCREEN_W as i32,
                    ACTION_BAR_H,
                    false,
                );
            }
            Action::Refine => {
                *snapshot = Some(surface.copy_rect(0, 0, SCREEN_W, SCREEN_H));
                let export = if base.is_some() {
                    write_canvas_png(surface, &page_path, SCREEN_H - ACTION_BAR_H as usize)
                } else {
                    ink.to_png(surface, page_path.to_str().unwrap())
                };
                if let Err(error) = export {
                    state.refine_finished(Err(error.to_string()));
                } else {
                    start_refine(net_tx.clone(), page_path.clone(), refined_path.clone());
                }
                render_controls(surface, &font, state);
                display.update(
                    0,
                    SCREEN_H as i32 - ACTION_BAR_H,
                    SCREEN_W as i32,
                    ACTION_BAR_H,
                    false,
                );
            }
            Action::Poll { after } => {
                start_poll(net_tx.clone(), after);
                render_controls(surface, &font, state);
                display.update(
                    0,
                    SCREEN_H as i32 - ACTION_BAR_H,
                    SCREEN_W as i32,
                    ACTION_BAR_H,
                    false,
                );
            }
            Action::ClearCanvas => {
                ink.clear();
                snapshot.take();
                base.take();
                render_compose_shell(surface, &font);
                render_controls(surface, &font, state);
                display.full_refresh(SCREEN_W, SCREEN_H);
                let _ = std::fs::remove_file(&page_path);
            }
            Action::DismissReply | Action::DismissRefinement => {
                if let Some(saved) = snapshot.take() {
                    surface.paste_rect(0, 0, SCREEN_W, SCREEN_H, &saved);
                } else {
                    render_compose_shell(surface, &font);
                }
                render_controls(surface, &font, state);
                display.full_refresh(SCREEN_W, SCREEN_H);
            }
            Action::AdoptRefinement => {
                ink.clear();
                let canvas_height = SCREEN_H - ACTION_BAR_H as usize;
                *base = Some(surface.copy_rect(0, 0, SCREEN_W, canvas_height));
                snapshot.take();
                state.has_ink = true;
                render_controls(surface, &font, state);
                display.full_refresh(SCREEN_W, SCREEN_H);
            }
            Action::None | Action::Draw => {
                render_controls(surface, &font, state);
                display.update(
                    0,
                    SCREEN_H as i32 - ACTION_BAR_H,
                    SCREEN_W as i32,
                    ACTION_BAR_H,
                    false,
                );
            }
        }
    };

    while !stopping.load(Ordering::Relaxed) {
        let now_ms = started.elapsed().as_millis() as u64;
        while let Ok(result) = net_rx.try_recv() {
            match result {
                NetResult::Upload(result) => {
                    let action = state.upload_finished(result);
                    apply_action(
                        action,
                        &mut state,
                        &mut ink,
                        &mut surface,
                        &display,
                        &mut compose_snapshot,
                        &mut base_snapshot,
                    );
                }
                NetResult::Refine(result) => {
                    let result = result.and_then(|()| {
                        render_refined_png(&mut surface, &refined_path)
                            .map_err(|error| error.to_string())
                    });
                    state.refine_finished(result);
                    render_controls(&mut surface, &font, &state);
                    if matches!(state.screen, Screen::Refined) {
                        display.full_refresh(SCREEN_W, SCREEN_H);
                    } else {
                        display.update(
                            0,
                            SCREEN_H as i32 - ACTION_BAR_H,
                            SCREEN_W as i32,
                            ACTION_BAR_H,
                            false,
                        );
                    }
                }
                NetResult::Poll(result) => {
                    let result = result.map(|reply| reply.map(|reply| (reply.id, reply.body)));
                    if result
                        .as_ref()
                        .ok()
                        .and_then(|reply| reply.as_ref())
                        .is_some()
                    {
                        compose_snapshot = Some(surface.copy_rect(0, 0, SCREEN_W, SCREEN_H));
                    }
                    state.poll_finished(result);
                    if let Screen::Reply { id, body } = &state.screen {
                        write_last_reply(&outbox, *id);
                        render_reply(&mut surface, &display, &font, body);
                    }
                    render_controls(&mut surface, &font, &state);
                    display.update(
                        0,
                        SCREEN_H as i32 - ACTION_BAR_H,
                        SCREEN_W as i32,
                        ACTION_BAR_H,
                        false,
                    );
                }
            }
        }

        for sample in pen.drain() {
            let touching = sample.touching && sample.pressure > 40;
            if touching && !pen_down {
                pen_down = true;
                press_target = MailboxState::target(sample.x, sample.y);
            }
            if touching {
                if matches!(state.screen, Screen::Compose)
                    && press_target == ComposeTarget::Canvas
                    && MailboxState::target(sample.x, sample.y) == ComposeTarget::Canvas
                {
                    let changed = match sample.tool {
                        Tool::Pen => {
                            let radius = 2 + sample.pressure * 3 / MAX_PRESSURE;
                            ink.pen_point(&mut surface, sample.x, sample.y, radius)
                        }
                        Tool::Eraser => {
                            let changed = ink.erase_point(&mut surface, sample.x, sample.y, 22);
                            if let Some(base) = &base_snapshot {
                                let canvas_height = SCREEN_H - ACTION_BAR_H as usize;
                                let x0 = changed.x0.max(0) as usize;
                                let y0 = changed.y0.max(0) as usize;
                                let x1 = (changed.x1.max(0) as usize + 1).min(SCREEN_W);
                                let y1 = (changed.y1.max(0) as usize + 1).min(canvas_height);
                                if x1 > x0 && y1 > y0 {
                                    surface.paste_from_snapshot(
                                        x0,
                                        y0,
                                        x1 - x0,
                                        y1 - y0,
                                        SCREEN_W,
                                        canvas_height,
                                        base,
                                    );
                                    ink.render_region(&mut surface, changed);
                                }
                            }
                            changed
                        }
                    };
                    if !changed.is_empty() {
                        dirty.add(changed.x0, changed.y0, 0);
                        dirty.add(changed.x1, changed.y1, 0);
                    }
                }
                continue;
            }
            if pen_down {
                pen_down = false;
                ink.pen_up();
                let release_target = MailboxState::target(sample.x, sample.y);
                let screen_before = state.screen.clone();
                let action = if matches!(state.screen, Screen::Compose) {
                    if press_target == ComposeTarget::Canvas {
                        state.has_ink = base_snapshot.is_some() || !ink.is_empty();
                        Action::None
                    } else if press_target == release_target {
                        state.tap(sample.x, sample.y, now_ms)
                    } else {
                        Action::None
                    }
                } else {
                    state.tap(sample.x, sample.y, now_ms)
                };
                if action != Action::None || state.screen != screen_before {
                    apply_action(
                        action,
                        &mut state,
                        &mut ink,
                        &mut surface,
                        &display,
                        &mut compose_snapshot,
                        &mut base_snapshot,
                    );
                }
                press_target = ComposeTarget::Outside;
            }
        }
        if pen.take_quit_requested() {
            break;
        }
        if !dirty.is_empty() && last_flush.elapsed() >= Duration::from_millis(8) {
            let (x, y, w, h) = dirty.rect();
            display.update(x, y, w, h, true);
            dirty = BBox::empty();
            last_flush = Instant::now();
        }
        let action = if pen_down {
            Action::None
        } else {
            state.tick(now_ms)
        };
        if action != Action::None {
            apply_action(
                action,
                &mut state,
                &mut ink,
                &mut surface,
                &display,
                &mut compose_snapshot,
                &mut base_snapshot,
            );
        }
        let _ = display.pump();
        std::thread::sleep(Duration::from_millis(4));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::{Font, FontRef};

    fn center(target: ComposeTarget) -> (i32, i32) {
        let y = SCREEN_H as i32 - ACTION_BAR_H / 2;
        match target {
            ComposeTarget::Send => (SCREEN_W as i32 / 8, y),
            ComposeTarget::Refine => (SCREEN_W as i32 * 3 / 8, y),
            ComposeTarget::Inbox => (SCREEN_W as i32 * 5 / 8, y),
            ComposeTarget::Clear => (SCREEN_W as i32 * 7 / 8, y),
            ComposeTarget::Canvas => (SCREEN_W as i32 / 2, 200),
            ComposeTarget::Outside => (-1, -1),
        }
    }

    fn confirm() -> (i32, i32) {
        (SCREEN_W as i32 * 3 / 4, SCREEN_H as i32 * 5 / 6)
    }

    #[test]
    fn action_strip_is_outside_the_drawing_canvas() {
        assert_eq!(
            MailboxState::target(center(ComposeTarget::Canvas).0, 200),
            ComposeTarget::Canvas
        );
        for target in [
            ComposeTarget::Send,
            ComposeTarget::Refine,
            ComposeTarget::Inbox,
            ComposeTarget::Clear,
        ] {
            let (x, y) = center(target);
            assert_eq!(MailboxState::target(x, y), target);
        }
    }

    #[test]
    fn send_requires_ink_and_two_deliberate_taps() {
        let mut state = MailboxState::new(0, 0);
        let (x, y) = center(ComposeTarget::Send);
        assert_eq!(state.tap(x, y, 0), Action::None);
        assert_eq!(state.screen, Screen::Compose);
        state.note_ink();
        assert_eq!(state.tap(x, y, 0), Action::None);
        assert_eq!(state.screen, Screen::ConfirmSend);
        let (x, y) = confirm();
        assert_eq!(state.tap(x, y, 0), Action::Upload);
        assert_eq!(state.screen, Screen::Sending);
    }

    #[test]
    fn tap_outside_confirmation_cancels_send() {
        let mut state = MailboxState::new(0, 0);
        state.note_ink();
        let (x, y) = center(ComposeTarget::Send);
        state.tap(x, y, 0);
        assert_eq!(state.tap(10, 10, 0), Action::None);
        assert_eq!(state.screen, Screen::Compose);
        assert!(state.has_ink);
    }

    #[test]
    fn refine_requires_confirmation_preserves_ink_and_can_be_dismissed_or_retried() {
        let mut state = MailboxState::new(0, 0);
        let (x, y) = center(ComposeTarget::Refine);
        assert_eq!(state.tap(x, y, 0), Action::None);
        assert_eq!(state.screen, Screen::Compose);

        state.note_ink();
        assert_eq!(state.tap(x, y, 0), Action::None);
        assert_eq!(state.screen, Screen::ConfirmRefine);
        let (x, y) = confirm();
        assert_eq!(state.tap(x, y, 0), Action::Refine);
        assert_eq!(state.screen, Screen::Refining);
        assert_eq!(state.refine_finished(Ok(())), Action::None);
        assert_eq!(state.screen, Screen::Refined);
        assert!(state.has_ink);
        assert_eq!(state.tap(200, 200, 1), Action::DismissRefinement);
        assert_eq!(state.screen, Screen::Compose);
        assert!(state.has_ink);

        state.screen = Screen::Refined;
        let (x, y) = confirm();
        assert_eq!(state.tap(x, y, 1), Action::AdoptRefinement);
        assert_eq!(state.screen, Screen::Compose);
        assert!(state.has_ink);

        state.screen = Screen::Refining;
        state.refine_finished(Err("offline".into()));
        assert_eq!(
            state.screen,
            Screen::Error {
                message: "offline".into(),
                retry: Retry::Refine,
            }
        );
        let (x, y) = confirm();
        assert_eq!(state.tap(x, y, 2), Action::Refine);
        assert_eq!(state.screen, Screen::Refining);
    }

    #[test]
    fn failed_upload_preserves_drawing_and_can_retry() {
        let mut state = MailboxState::new(0, 0);
        state.note_ink();
        state.screen = Screen::Sending;
        assert_eq!(state.upload_finished(Err("offline".into())), Action::None);
        assert!(state.has_ink);
        assert_eq!(
            state.screen,
            Screen::Error {
                message: "offline".into(),
                retry: Retry::Upload
            }
        );
        let (x, y) = confirm();
        assert_eq!(state.tap(x, y, 10), Action::Upload);
    }

    #[test]
    fn successful_upload_clears_only_after_acknowledgement() {
        let mut state = MailboxState::new(0, 0);
        state.note_ink();
        state.screen = Screen::Sending;
        assert!(state.has_ink);
        assert_eq!(state.upload_finished(Ok(12)), Action::ClearCanvas);
        assert!(!state.has_ink);
        assert_eq!(state.screen, Screen::Compose);
    }

    #[test]
    fn clear_requires_confirmation() {
        let mut state = MailboxState::new(0, 0);
        state.note_ink();
        let (x, y) = center(ComposeTarget::Clear);
        assert_eq!(state.tap(x, y, 0), Action::None);
        assert_eq!(state.screen, Screen::ConfirmClear);
        let (x, y) = confirm();
        assert_eq!(state.tap(x, y, 0), Action::ClearCanvas);
        assert!(!state.has_ink);
    }

    #[test]
    fn inbox_poll_records_and_dismisses_reply() {
        let mut state = MailboxState::new(4, 0);
        let (x, y) = center(ComposeTarget::Inbox);
        assert_eq!(state.tap(x, y, 10), Action::Poll { after: 4 });
        state.poll_finished(Ok(Some((5, "Hello Ian".into()))));
        assert_eq!(state.last_reply_id, 5);
        assert_eq!(
            state.screen,
            Screen::Reply {
                id: 5,
                body: "Hello Ian".into()
            }
        );
        assert_eq!(state.tap(200, 200, 20), Action::DismissReply);
        assert_eq!(state.screen, Screen::Compose);
    }

    #[test]
    fn idle_tick_may_poll_but_never_uploads() {
        let mut state = MailboxState::new(8, 0);
        state.note_ink();
        assert_eq!(state.tick(59_999), Action::None);
        assert_eq!(state.tick(60_000), Action::Poll { after: 8 });
        assert!(state.has_ink);
    }

    #[test]
    fn decode_png_luma_converts_rgb_pixels_for_eink() {
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[255, 0, 0, 255, 255, 255])
                .unwrap();
        }
        let (width, height, pixels) = decode_png_luma(&encoded).unwrap();
        assert_eq!((width, height), (2, 1));
        assert!((75..=78).contains(&pixels[0]));
        assert_eq!(pixels[1], 255);
    }

    #[test]
    fn decode_png_luma_rejects_unsafe_dimensions_before_allocating() {
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 5000, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&vec![255; 5000]).unwrap();
        }
        let error = decode_png_luma(&encoded).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("dimensions"));
    }

    #[test]
    fn full_canvas_export_keeps_ai_background_and_overlay() {
        use crate::surface::{PixFmt, Surface, BLACK, WHITE};
        let mut bytes = vec![255u8; 12 * 10];
        let mut surface = Surface::new(bytes.as_mut_ptr(), bytes.len(), 12, 10, 12, PixFmt::Gray8);
        surface.fill_rect(0, 0, 12, 8, WHITE);
        surface.fill_rect(2, 2, 4, 3, BLACK);
        surface.fill_rect(9, 6, 2, 2, BLACK);
        let path = std::env::temp_dir().join(format!(
            "mailbox-canvas-export-{}-{}.png",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        write_canvas_png(&surface, &path, 8).unwrap();
        let png = std::fs::read(&path).unwrap();
        let (width, height, decoded) = decode_png_luma(&png).unwrap();
        assert_eq!((width, height), (12, 8));
        assert_eq!(decoded[2 * width + 2], 0);
        assert_eq!(decoded[6 * width + 9], 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bundled_mailbox_font_contains_chinese_glyphs() {
        let font = FontRef::try_from_slice(MAILBOX_FONT_TTF).unwrap();
        assert_ne!(font.glyph_id('纸').0, 0);
        assert_ne!(font.glyph_id('飞').0, 0);
    }
}
