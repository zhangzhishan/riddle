# Paper Plane Mailbox verification

Verified on 2026-07-17 from `/Users/zhishan/code/ian-paper-mailbox`.

## Automated tests

- `cargo test`
  - Result: **43 passed, 0 failed**.
  - Includes deliberate two-tap send, cancellation, clear confirmation, offline upload preservation/retry, inbox polling, UTF-8 protocol handling, and bundled Chinese glyph coverage.
- `python3 -m unittest -q server/test_mailbox_server.py`
  - Result: **27 passed, 0 failed**.
  - Covers SQLite persistence, strict routes/methods/fragments, separate family/device authentication, safe cookie exchange, PNG validation including malformed duplicate headers, 4 MiB limits, protected images/CSS, HTML escaping, duplicate/oversized replies, strict request framing, 204 polling, and no-store responses.
- `sh -n kobo/launch.sh kobo/install.sh kobo/uninstall.sh kobo/restore-nickel.sh`
  - Result: pass.
- `bash -n scripts/build-kobo.sh`
  - Result: pass.

## End-to-end local loop

A real local server was started with temporary tokens and storage. The compiled Rust diagnostic client then performed the same HTTP operations as the Kobo UI:

1. Uploaded a valid raw PNG through `--mailbox-send`.
2. Opened the family capability URL and exchanged it for the protected cookie.
3. Confirmed the uploaded drawing appeared in the family page.
4. Posted the UTF-8 reply `Ian，你的纸飞机到了！`.
5. Retrieved the exact text and reply ID through `--mailbox-check`.
6. Confirmed anonymous image access returned HTTP 403.

Result:

```text
E2E PASS: message=2, reply_id=2, utf8=Ian，你的纸飞机到了！, anonymous_image=403
```

## Browser QA

The family capability URL was opened in a real browser session. The responsive page rendered the uploaded image and Chinese controls without JavaScript or console errors. A Chinese reply containing emoji was entered and submitted through the visible form; after redirect it appeared correctly under `家人的回信` with preserved punctuation and emoji.

No functional or visual blocker was found in the desktop-width browser pass. The CSS uses a fluid `min(100% - 1.5rem, 42rem)` container, wrapped metadata, full-width textarea/button, and a 640 px breakpoint; a physical-phone pass remains part of deployment verification.

## Kobo cross-build

Command: `./scripts/build-kobo.sh`

Result:

```text
dist/riddle-kobo/.adds/riddle-kobo/riddle:
ELF 32-bit LSB executable, ARM, EABI5, statically linked, stripped
```

The staged bundle contains:

- the ARM Kobo binary;
- reversible Nickel launcher and restore helper;
- both `Riddle Diary` and `Paper Plane Mailbox` NickelMenu entries;
- configuration example;
- Ma Shan Zheng SIL OFL license.

Artifact:

```text
dist/riddle-kobo.zip
bytes: 5,448,322
sha256: eb3fd317c64859512049816b0c2b510028686e4bca75c7341891ca5ff5e267b2
```

## Still requires real-device verification

Host and cross-build verification cannot prove physical behavior. Before calling the app production-ready, verify on the user's Kobo Elipsa 2E:

- action-strip hit targets and mirrored stylus coordinates;
- ink latency and partial-refresh ghosting;
- Chinese handwriting animation appearance and line spacing;
- Wi-Fi reconnect after sleep;
- upload retry across a real network outage;
- safe Nickel restoration after normal exit, SIGTERM, and an app crash.

Public HTTPS deployment is also intentionally pending a hostname/hosting decision. The server currently runs plain HTTP and must sit behind a trusted TLS reverse proxy before internet exposure.
