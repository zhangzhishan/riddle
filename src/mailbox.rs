//! Pure interaction model for the Paper Plane Mailbox.
//!
//! This module deliberately contains no framebuffer, stylus, or HTTP code.
//! The Kobo loop translates taps and network completions into these events,
//! which keeps accidental sends and error recovery host-testable.

use crate::fb::{SCREEN_H, SCREEN_W};

pub const ACTION_BAR_H: i32 = 180;
const AUTO_POLL_MS: u64 = 60_000;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
