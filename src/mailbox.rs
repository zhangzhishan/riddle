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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Compose,
    ConfirmSend,
    ConfirmClear,
    Sending,
    CheckingInbox,
    Reply { id: u64, body: String },
    Error { message: String, retry: Retry },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Draw,
    Upload,
    Poll { after: u64 },
    ClearCanvas,
    DismissReply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeTarget {
    Canvas,
    Send,
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
        let third = SCREEN_W as i32 / 3;
        if x < third {
            ComposeTarget::Send
        } else if x < third * 2 {
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
                    }
                } else {
                    self.screen = Screen::Compose;
                    Action::None
                }
            }
            Screen::Sending | Screen::CheckingInbox => Action::None,
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
                let third = SCREEN_W / 3;
                surf.fill_rect(third, y, 2, ACTION_BAR_H as usize, BLACK);
                surf.fill_rect(third * 2, y, 2, ACTION_BAR_H as usize, BLACK);
                draw_text(surf, font, "发送", 56.0, third / 2, y + 50);
                draw_text(surf, font, "收信", 56.0, third + third / 2, y + 50);
                draw_text(surf, font, "清空", 56.0, third * 2 + third / 2, y + 50);
            }
            Screen::ConfirmSend => {
                surf.fill_rect(SCREEN_W / 2, y, 2, ACTION_BAR_H as usize, BLACK);
                draw_text(surf, font, "取消", 52.0, SCREEN_W / 4, y + 50);
                draw_text(surf, font, "现在发送", 52.0, SCREEN_W * 3 / 4, y + 50);
            }
            Screen::ConfirmClear => {
                surf.fill_rect(SCREEN_W / 2, y, 2, ACTION_BAR_H as usize, BLACK);
                draw_text(surf, font, "取消", 52.0, SCREEN_W / 4, y + 50);
                draw_text(surf, font, "清空画纸", 52.0, SCREEN_W * 3 / 4, y + 50);
            }
            Screen::Sending => draw_text(surf, font, "正在折纸飞机……", 52.0, SCREEN_W / 2, y + 50),
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
    let started = Instant::now();
    let mut state = MailboxState::new(read_last_reply(&outbox), 0);
    let mut ink = Ink::new();
    let mut pen_down = false;
    let mut press_target = ComposeTarget::Outside;
    let mut dirty = BBox::empty();
    let mut last_flush = Instant::now();
    let mut compose_snapshot: Option<Vec<u8>> = None;
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
                        snapshot: &mut Option<Vec<u8>>| {
        match action {
            Action::Upload => {
                if let Err(error) = ink.to_png(surface, page_path.to_str().unwrap()) {
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
                render_compose_shell(surface, &font);
                render_controls(surface, &font, state);
                display.full_refresh(SCREEN_W, SCREEN_H);
                let _ = std::fs::remove_file(&page_path);
            }
            Action::DismissReply => {
                if let Some(saved) = snapshot.take() {
                    surface.paste_rect(0, 0, SCREEN_W, SCREEN_H, &saved);
                } else {
                    render_compose_shell(surface, &font);
                }
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
                    );
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
                        Tool::Eraser => ink.erase_point(&mut surface, sample.x, sample.y, 22),
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
                        state.has_ink = !ink.is_empty();
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
            ComposeTarget::Send => (SCREEN_W as i32 / 6, y),
            ComposeTarget::Inbox => (SCREEN_W as i32 / 2, y),
            ComposeTarget::Clear => (SCREEN_W as i32 * 5 / 6, y),
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
    fn bundled_mailbox_font_contains_chinese_glyphs() {
        let font = FontRef::try_from_slice(MAILBOX_FONT_TTF).unwrap();
        assert_ne!(font.glyph_id('纸').0, 0);
        assert_ne!(font.glyph_id('飞').0, 0);
    }
}
