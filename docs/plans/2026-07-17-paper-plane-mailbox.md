# Paper Plane Mailbox Implementation Plan

> **For Hermes:** Use subagent-driven-development skill to implement this plan task-by-task.

**Goal:** Build a private family mailbox where Ian draws a page on Kobo Elipsa 2E, sends it deliberately, family members view it and reply in a mobile browser, and the reply appears on the Kobo as animated handwriting.

**Architecture:** Keep the Kobo device as a thin client and reuse the existing FBInk, stylus, surface, ink, PNG, launcher, and handwritten-text renderer. Add a small Python-standard-library server using `http.server` and SQLite: the device uploads raw PNG and polls for one pending UTF-8 reply; family access uses a separate capability token and a responsive server-rendered page. The MVP has no user accounts, no public gallery, no AI, and no automatic sending.

**Tech Stack:** Rust 2021 + ureq on Kobo; Python 3.11 standard library (`http.server`, `sqlite3`, `unittest`); HTML/CSS; existing FBInk/NickelMenu packaging.

---

## Product rules

- Sending must be deliberate. Pausing the pen never sends a page.
- The bottom action strip is outside the drawing canvas and has `Send`, `Inbox`, and `Clear` targets operated with the stylus.
- Tapping `Send` once opens a confirmation sheet; tapping the confirmation target sends. Tapping elsewhere cancels.
- Failed uploads preserve the drawing and show a retryable error.
- A successful upload clears the drawing only after the server acknowledges it.
- `Inbox` polls manually; while the compose screen is idle the client may also poll every 60 seconds.
- The server stores the original PNG and reply text. Device and family tokens are different.
- Family pages and image URLs require the family token; device routes require the device token.
- Tokens are read from environment/config and are never committed.

## HTTP protocol

### Device routes

- `POST /api/device/messages`
  - Header: `Authorization: Bearer <device token>`
  - Header: `Content-Type: image/png`
  - Header: `X-Device-Id: ian-kobo`
  - Body: raw PNG, maximum 4 MiB
  - Success: `201`, body is decimal message ID
- `GET /api/device/replies?after=<reply-id>`
  - Header: `Authorization: Bearer <device token>`
  - No new reply: `204`
  - New reply: `200`, header `X-Reply-Id`, UTF-8 text body

### Family routes

- `GET /?token=<family token>`: responsive message list; sets an HttpOnly SameSite cookie and redirects to `/` without the token in the URL.
- `GET /messages/<id>.png`: protected original drawing.
- `POST /messages/<id>/reply`: protected form post with UTF-8 text, maximum 2,000 characters.
- `GET /healthz`: unprivileged liveness response containing no private data.

## Storage

SQLite tables:

```sql
CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  device_id TEXT NOT NULL,
  image_path TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE replies (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id INTEGER NOT NULL UNIQUE REFERENCES messages(id) ON DELETE CASCADE,
  body TEXT NOT NULL,
  created_at TEXT NOT NULL
);
```

Images live in `MAILBOX_DATA_DIR/images/<message-id>.png`; SQLite lives at `MAILBOX_DATA_DIR/mailbox.sqlite3`.

---

### Task 1: Add the tested server core

**Objective:** Persist uploaded PNG messages and replies using only Python's standard library.

**Files:**
- Create: `server/mailbox_server.py`
- Create: `server/test_mailbox_server.py`
- Create: `server/README.md`
- Create: `server/.gitignore`

**Steps:**
1. Write failing unit tests for database initialization, PNG validation/size limits, upload persistence, reply creation, and next-reply lookup.
2. Run `python3 -m unittest -v server/test_mailbox_server.py` and confirm failure.
3. Implement `MailboxStore` with parameterized SQLite operations and atomic image writes.
4. Run the tests and confirm they pass.
5. Commit: `feat: add paper plane mailbox storage`.

### Task 2: Add authenticated HTTP routes and mobile web UI

**Objective:** Expose the exact device protocol and a phone-friendly private family page.

**Files:**
- Modify: `server/mailbox_server.py`
- Modify: `server/test_mailbox_server.py`
- Create: `server/static/style.css`

**Steps:**
1. Add failing HTTP integration tests for bearer authentication, family-token cookie exchange, upload, protected image retrieval, form reply, 204 polling, and 4 MiB rejection.
2. Implement a `ThreadingHTTPServer` handler with strict route matching, constant-time token comparison, body/time limits, escaped HTML, HttpOnly SameSite cookie, and no-store private responses.
3. Render a single-column mobile page showing the latest drawings first, existing replies, and one reply form per unanswered message.
4. Run `python3 -m unittest -v server/test_mailbox_server.py`.
5. Start the server on an ephemeral local port and exercise `/healthz` with `curl`.
6. Commit: `feat: add private family mailbox web app`.

### Task 3: Add a host-testable Rust mailbox protocol client

**Objective:** Upload raw PNG and poll UTF-8 replies without coupling networking to the UI loop.

**Files:**
- Create: `src/mailbox_client.rs`
- Modify: `src/main.rs`
- Modify: `Cargo.toml` only if a test-only dependency is unavoidable.

**Steps:**
1. Add tests using a local `TcpListener` mock server for request path, bearer header, raw PNG body, decimal message ID parsing, 204 polling, reply ID parsing, UTF-8 body handling, timeouts, and non-2xx errors.
2. Implement `MailboxClient::from_env`, `send_png`, and `poll_reply` with `MAILBOX_BASE_URL`, `MAILBOX_DEVICE_TOKEN`, and `MAILBOX_DEVICE_ID`.
3. Add diagnostics: `--mailbox-send <PNG>` and `--mailbox-check [after-id]`.
4. Run `cargo test` and `cargo test --features kobo --no-run`.
5. Commit: `feat: add Kobo mailbox protocol client`.

### Task 4: Add deliberate compose/send/inbox state logic

**Objective:** Make the child-facing interaction testable independently of FBInk and network timing.

**Files:**
- Create: `src/mailbox.rs`
- Modify: `src/main.rs`
- Modify: `src/kobo_pen.rs` if a button event must be exposed.

**Steps:**
1. Write state-machine tests for drawing-area bounds, send confirmation, cancel, clear confirmation, upload success/failure, manual inbox polling, reply dismissal, and no idle auto-send.
2. Implement pure `MailboxState` transitions and action outputs (`Draw`, `Erase`, `ConfirmSend`, `Upload`, `Poll`, `Clear`, `DismissReply`, `Exit`).
3. Add `riddle --mailbox` dispatch without changing the existing diary default.
4. Run focused and full Rust tests.
5. Commit: `feat: add paper plane mailbox interaction model`.

### Task 5: Render and run the Kobo mailbox UI

**Objective:** Provide a complete FBInk/stylus application using the tested state and protocol layers.

**Files:**
- Modify: `src/mailbox.rs`
- Modify: `src/script.rs` only for reusable text layout if necessary.
- Modify: `kobo/launch.sh`
- Modify: `kobo/nm/riddle-kobo`
- Modify: `scripts/build-kobo.sh`
- Modify: `README.md`

**Steps:**
1. Render a white compose page with a narrow title/status area and bottom `SEND / INBOX / CLEAR` strip.
2. Feed stylus samples to `Ink`; reserve the action strip so it never enters the PNG.
3. Export PNG to a durable outbox path, upload on a worker thread, and clear only on acknowledged success.
4. Poll replies on a worker thread and animate received text with the existing handwriting tracing code.
5. Keep failed uploads in an outbox for retry after restart.
6. Add a separate NickelMenu entry `Paper Plane Mailbox` that launches `riddle --mailbox` through the existing reversible launcher.
7. Run host tests, shell syntax checks, and ARMv7 cross-build.
8. Commit: `feat: add Kobo paper plane mailbox UI`.

### Task 6: End-to-end verification and delivery

**Objective:** Prove the complete local loop and prepare safe deployment/device verification.

**Files:**
- Create: `docs/paper-plane-mailbox-verification.md`
- Modify: `README.md`

**Steps:**
1. Start the server with temporary data and generated test tokens.
2. Use `--mailbox-send` to upload a fixture PNG.
3. Open the family page in a browser-sized viewport, post a reply, and verify `--mailbox-check` retrieves the exact UTF-8 text once.
4. Verify unauthorized routes return 401/403 and private responses are not cacheable.
5. Run `cargo test`, Python tests, shell syntax checks, and Kobo cross-build.
6. Commit final docs and push `feature/paper-plane-mailbox` to `origin`.
7. Defer public HTTPS deployment and real-device interaction verification until the user confirms the desired hostname/hosting target and the Kobo is available.
