#!/usr/bin/env python3
"""Private Paper Plane Mailbox server using only Python's standard library."""

from datetime import datetime, timezone
import contextlib
import hashlib
import hmac
import html
from http import HTTPStatus
from http.cookies import CookieError, SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import re
import sqlite3
import struct
import sys
import tempfile
import urllib.parse
import zlib


MAX_IMAGE_BYTES = 4 * 1024 * 1024
MAX_REPLY_CHARACTERS = 2000
MAX_FORM_BYTES = 32 * 1024
READ_TIMEOUT_SECONDS = 15
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


class ValidationError(ValueError):
    """Input did not satisfy mailbox constraints."""


class ReplyAlreadyExists(ValidationError):
    """A message already has its single allowed reply."""


def _utc_timestamp():
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def _tokens_equal(supplied, expected):
    return hmac.compare_digest(
        supplied.encode("utf-8"), expected.encode("utf-8")
    )


def _validate_png(payload):
    if not isinstance(payload, bytes):
        raise ValidationError("image must be PNG bytes")
    if len(payload) > MAX_IMAGE_BYTES:
        raise ValidationError("image exceeds the 4 MiB limit")
    if not payload.startswith(PNG_SIGNATURE):
        raise ValidationError("invalid PNG signature")

    offset = len(PNG_SIGNATURE)
    first_chunk = True
    saw_idat = False
    saw_iend = False
    while offset < len(payload):
        if len(payload) - offset < 12:
            raise ValidationError("invalid PNG chunk")
        length = struct.unpack(">I", payload[offset : offset + 4])[0]
        chunk_end = offset + 12 + length
        if chunk_end > len(payload):
            raise ValidationError("invalid PNG chunk length")
        kind = payload[offset + 4 : offset + 8]
        data = payload[offset + 8 : offset + 8 + length]
        expected_crc = struct.unpack(">I", payload[offset + 8 + length : chunk_end])[0]
        if (zlib.crc32(kind + data) & 0xFFFFFFFF) != expected_crc:
            raise ValidationError("invalid PNG chunk checksum")
        if first_chunk and (kind != b"IHDR" or length != 13):
            raise ValidationError("invalid PNG header")
        if kind == b"IHDR":
            width, height = struct.unpack(">II", data[:8])
            if width == 0 or height == 0:
                raise ValidationError("invalid PNG dimensions")
        elif kind == b"IDAT":
            saw_idat = True
        elif kind == b"IEND":
            if length != 0 or chunk_end != len(payload):
                raise ValidationError("invalid PNG ending")
            saw_iend = True
        offset = chunk_end
        first_chunk = False
        if saw_iend:
            break

    if not saw_idat or not saw_iend:
        raise ValidationError("incomplete PNG")


class MailboxStore:
    """SQLite metadata and atomically-written PNG storage."""

    def __init__(self, data_dir):
        self.data_dir = Path(data_dir)
        self.images_dir = self.data_dir / "images"
        self.database_path = self.data_dir / "mailbox.sqlite3"
        self.images_dir.mkdir(parents=True, exist_ok=True)
        self._initialize_database()

    def _connect(self):
        connection = sqlite3.connect(str(self.database_path), timeout=30)
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA foreign_keys = ON")
        return connection

    def _initialize_database(self):
        with self._connect() as connection:
            connection.executescript(
                """
                CREATE TABLE IF NOT EXISTS messages (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  device_id TEXT NOT NULL,
                  image_path TEXT NOT NULL,
                  created_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS replies (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  message_id INTEGER NOT NULL UNIQUE REFERENCES messages(id) ON DELETE CASCADE,
                  body TEXT NOT NULL,
                  created_at TEXT NOT NULL
                );
                """
            )

    def create_message(self, device_id, payload):
        if not isinstance(device_id, str) or not device_id.strip():
            raise ValidationError("device ID must not be empty")
        _validate_png(payload)

        connection = self._connect()
        temporary_path = None
        final_path = None
        try:
            connection.execute("BEGIN IMMEDIATE")
            cursor = connection.execute(
                "INSERT INTO messages (device_id, image_path, created_at) VALUES (?, ?, ?)",
                (device_id, "", _utc_timestamp()),
            )
            message_id = cursor.lastrowid
            relative_path = "images/{}.png".format(message_id)
            final_path = self.data_dir / relative_path
            connection.execute(
                "UPDATE messages SET image_path = ? WHERE id = ?",
                (relative_path, message_id),
            )

            descriptor, name = tempfile.mkstemp(
                prefix=".{}-".format(message_id), suffix=".tmp", dir=str(self.images_dir)
            )
            temporary_path = Path(name)
            with os.fdopen(descriptor, "wb") as image_file:
                image_file.write(payload)
                image_file.flush()
                os.fsync(image_file.fileno())
            os.replace(str(temporary_path), str(final_path))
            temporary_path = None
            self._sync_images_directory()
            connection.commit()
            return message_id
        except Exception:
            connection.rollback()
            if final_path is not None:
                with contextlib.suppress(FileNotFoundError):
                    final_path.unlink()
            raise
        finally:
            if temporary_path is not None:
                with contextlib.suppress(FileNotFoundError):
                    temporary_path.unlink()
            connection.close()

    def _sync_images_directory(self):
        descriptor = os.open(str(self.images_dir), os.O_RDONLY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)

    def create_reply(self, message_id, body):
        if not isinstance(body, str):
            raise ValidationError("reply must be text")
        if not body:
            raise ValidationError("reply must not be empty")
        if len(body) > MAX_REPLY_CHARACTERS:
            raise ValidationError("reply must be at most 2,000 characters")

        with self._connect() as connection:
            exists = connection.execute(
                "SELECT 1 FROM messages WHERE id = ?", (message_id,)
            ).fetchone()
            if exists is None:
                raise ValidationError("message does not exist")
            try:
                cursor = connection.execute(
                    "INSERT INTO replies (message_id, body, created_at) VALUES (?, ?, ?)",
                    (message_id, body, _utc_timestamp()),
                )
            except sqlite3.IntegrityError as error:
                if "UNIQUE" in str(error).upper():
                    raise ReplyAlreadyExists("message already has a reply") from error
                raise
            return cursor.lastrowid

    def next_reply(self, after_reply_id):
        with self._connect() as connection:
            row = connection.execute(
                """
                SELECT id, message_id, body
                FROM replies
                WHERE id > ?
                ORDER BY id ASC
                LIMIT 1
                """,
                (after_reply_id,),
            ).fetchone()
        return dict(row) if row is not None else None

    def list_messages(self):
        with self._connect() as connection:
            rows = connection.execute(
                """
                SELECT messages.id, messages.device_id, messages.image_path,
                       messages.created_at, replies.id AS reply_id,
                       replies.body AS reply_body, replies.created_at AS reply_created_at
                FROM messages
                LEFT JOIN replies ON replies.message_id = messages.id
                ORDER BY messages.id DESC
                """
            ).fetchall()
        return [dict(row) for row in rows]

    def image_path_for_message(self, message_id):
        with self._connect() as connection:
            row = connection.execute(
                "SELECT image_path FROM messages WHERE id = ?", (message_id,)
            ).fetchone()
        if row is None:
            return None
        path = self.data_dir / row["image_path"]
        return path if path.is_file() else None


class MailboxHTTPServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


_IMAGE_ROUTE = re.compile(r"/messages/([1-9][0-9]*)\.png")
_REPLY_ROUTE = re.compile(r"/messages/([1-9][0-9]*)/reply")


def create_server(host, port, store, device_token, family_token):
    if not device_token or not family_token:
        raise ValueError("device and family tokens must not be empty")
    if _tokens_equal(device_token, family_token):
        raise ValueError("device and family tokens must be different")

    family_cookie_value = hmac.new(
        family_token.encode("utf-8"), b"paper-plane-family-session", hashlib.sha256
    ).hexdigest()
    static_path = Path(__file__).resolve().parent / "static" / "style.css"

    class MailboxRequestHandler(BaseHTTPRequestHandler):
        server_version = "PaperPlaneMailbox"
        sys_version = ""

        def setup(self):
            super().setup()
            self.connection.settimeout(READ_TIMEOUT_SECONDS)

        def log_message(self, format, *args):
            # Preserve normal http.server logging while avoiding query-string token leakage.
            safe_request = self.requestline.split("?", 1)[0]
            print("{} - - [{}] {}".format(
                self.client_address[0], self.log_date_time_string(),
                format % ((safe_request,) + args[1:] if args else args),
            ), file=sys.stderr)

        def do_GET(self):
            parsed = urllib.parse.urlsplit(self.path)
            if parsed.path == "/healthz" and not parsed.query and not parsed.fragment:
                self._respond(HTTPStatus.OK, b"ok\n", "text/plain; charset=utf-8", private=False)
                return
            if parsed.path == "/api/device/replies":
                self._handle_reply_poll(parsed)
                return
            if parsed.path == "/":
                self._handle_family_index(parsed)
                return
            image_match = _IMAGE_ROUTE.fullmatch(parsed.path)
            if image_match and not parsed.query and not parsed.fragment:
                self._handle_image(int(image_match.group(1)))
                return
            if parsed.path == "/static/style.css" and not parsed.query and not parsed.fragment:
                self._handle_static_css(static_path)
                return
            self._plain_error(HTTPStatus.NOT_FOUND, "not found")

        def do_POST(self):
            parsed = urllib.parse.urlsplit(self.path)
            if parsed.path == "/api/device/messages" and not parsed.query and not parsed.fragment:
                self._handle_upload()
                return
            reply_match = _REPLY_ROUTE.fullmatch(parsed.path)
            if reply_match and not parsed.query and not parsed.fragment:
                self._handle_reply_form(int(reply_match.group(1)))
                return
            self._plain_error(HTTPStatus.NOT_FOUND, "not found")

        def _handle_upload(self):
            if not self._device_authorized():
                self._unauthorized_device()
                return
            if self.headers.get("Transfer-Encoding") is not None:
                self._plain_error(HTTPStatus.BAD_REQUEST, "transfer encoding is not supported")
                return
            if self.headers.get("Content-Type", "").strip().lower() != "image/png":
                self._plain_error(HTTPStatus.UNSUPPORTED_MEDIA_TYPE, "Content-Type must be image/png")
                return
            device_id = self.headers.get("X-Device-Id")
            if device_id is None or not device_id.strip():
                self._plain_error(HTTPStatus.BAD_REQUEST, "X-Device-Id is required")
                return
            length = self._content_length(MAX_IMAGE_BYTES)
            if length is None:
                return
            try:
                payload = self.rfile.read(length)
            except (OSError, TimeoutError):
                self._plain_error(HTTPStatus.REQUEST_TIMEOUT, "request body timed out")
                return
            if len(payload) != length:
                self._plain_error(HTTPStatus.BAD_REQUEST, "incomplete request body")
                return
            try:
                message_id = store.create_message(device_id, payload)
            except ValidationError as error:
                self._plain_error(HTTPStatus.BAD_REQUEST, str(error))
                return
            self._respond(
                HTTPStatus.CREATED,
                str(message_id).encode("ascii"),
                "text/plain; charset=utf-8",
            )

        def _handle_reply_poll(self, parsed):
            if not self._device_authorized():
                self._unauthorized_device()
                return
            try:
                pairs = urllib.parse.parse_qsl(
                    parsed.query, keep_blank_values=True, strict_parsing=True
                )
            except ValueError:
                pairs = []
            if len(pairs) != 1 or pairs[0][0] != "after" or not pairs[0][1].isdigit():
                if not parsed.query:
                    self._plain_error(HTTPStatus.NOT_FOUND, "not found")
                else:
                    self._plain_error(HTTPStatus.BAD_REQUEST, "after must be a nonnegative decimal ID")
                return
            reply = store.next_reply(int(pairs[0][1]))
            if reply is None:
                self._respond(HTTPStatus.NO_CONTENT, b"", None)
                return
            self._respond(
                HTTPStatus.OK,
                reply["body"].encode("utf-8"),
                "text/plain; charset=utf-8",
                extra_headers=(("X-Reply-Id", str(reply["id"])),),
            )

        def _handle_family_index(self, parsed):
            if parsed.query:
                try:
                    pairs = urllib.parse.parse_qsl(
                        parsed.query, keep_blank_values=True, strict_parsing=True
                    )
                except ValueError:
                    pairs = []
                if (
                    len(pairs) != 1
                    or pairs[0][0] != "token"
                    or not _tokens_equal(pairs[0][1], family_token)
                ):
                    self._plain_error(HTTPStatus.FORBIDDEN, "forbidden")
                    return
                cookie = (
                    "mailbox_family={}; Path=/; HttpOnly; SameSite=Strict".format(
                        family_cookie_value
                    )
                )
                self._respond(
                    HTTPStatus.SEE_OTHER,
                    b"",
                    None,
                    extra_headers=(("Location", "/"), ("Set-Cookie", cookie)),
                )
                return
            if not self._family_authorized():
                self._plain_error(HTTPStatus.FORBIDDEN, "forbidden")
                return
            body = self._render_index().encode("utf-8")
            self._respond(
                HTTPStatus.OK,
                body,
                "text/html; charset=utf-8",
                extra_headers=(("Content-Security-Policy", "default-src 'self'; img-src 'self'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"),),
            )

        def _handle_image(self, message_id):
            if not self._family_authorized():
                self._plain_error(HTTPStatus.FORBIDDEN, "forbidden")
                return
            path = store.image_path_for_message(message_id)
            if path is None:
                self._plain_error(HTTPStatus.NOT_FOUND, "not found")
                return
            try:
                payload = path.read_bytes()
            except OSError:
                self._plain_error(HTTPStatus.NOT_FOUND, "not found")
                return
            self._respond(HTTPStatus.OK, payload, "image/png")

        def _handle_static_css(self, path):
            if not self._family_authorized():
                self._plain_error(HTTPStatus.FORBIDDEN, "forbidden")
                return
            try:
                payload = path.read_bytes()
            except OSError:
                self._plain_error(HTTPStatus.NOT_FOUND, "not found")
                return
            self._respond(HTTPStatus.OK, payload, "text/css; charset=utf-8")

        def _handle_reply_form(self, message_id):
            if not self._family_authorized():
                self._plain_error(HTTPStatus.FORBIDDEN, "forbidden")
                return
            content_type = self.headers.get("Content-Type", "").split(";", 1)[0].strip().lower()
            if content_type != "application/x-www-form-urlencoded":
                self._plain_error(
                    HTTPStatus.UNSUPPORTED_MEDIA_TYPE,
                    "Content-Type must be application/x-www-form-urlencoded",
                )
                return
            length = self._content_length(MAX_FORM_BYTES)
            if length is None:
                return
            try:
                encoded = self.rfile.read(length)
                form_text = encoded.decode("utf-8", "strict")
                pairs = urllib.parse.parse_qsl(
                    form_text, keep_blank_values=True, strict_parsing=True
                )
            except (OSError, TimeoutError):
                self._plain_error(HTTPStatus.REQUEST_TIMEOUT, "request body timed out")
                return
            except (UnicodeDecodeError, ValueError):
                self._plain_error(HTTPStatus.BAD_REQUEST, "invalid form body")
                return
            if len(pairs) != 1 or pairs[0][0] != "body":
                self._plain_error(HTTPStatus.BAD_REQUEST, "form requires one body field")
                return
            body = pairs[0][1]
            if not body.strip():
                self._plain_error(HTTPStatus.BAD_REQUEST, "reply must not be empty")
                return
            try:
                store.create_reply(message_id, body)
            except ReplyAlreadyExists as error:
                self._plain_error(HTTPStatus.CONFLICT, str(error))
                return
            except ValidationError as error:
                status = HTTPStatus.NOT_FOUND if "does not exist" in str(error) else HTTPStatus.BAD_REQUEST
                self._plain_error(status, str(error))
                return
            self._respond(
                HTTPStatus.SEE_OTHER,
                b"",
                None,
                extra_headers=(("Location", "/#message-{}".format(message_id)),),
            )

        def _content_length(self, maximum):
            raw_length = self.headers.get("Content-Length")
            if raw_length is None:
                self._plain_error(HTTPStatus.LENGTH_REQUIRED, "Content-Length is required")
                return None
            try:
                length = int(raw_length, 10)
            except ValueError:
                self._plain_error(HTTPStatus.BAD_REQUEST, "invalid Content-Length")
                return None
            if length < 0:
                self._plain_error(HTTPStatus.BAD_REQUEST, "invalid Content-Length")
                return None
            if length > maximum:
                label = "4 MiB" if maximum == MAX_IMAGE_BYTES else "form size"
                self._plain_error(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "request exceeds {} limit".format(label))
                return None
            return length

        def _device_authorized(self):
            authorization = self.headers.get("Authorization", "")
            prefix = "Bearer "
            if not authorization.startswith(prefix):
                return False
            supplied = authorization[len(prefix) :]
            return _tokens_equal(supplied, device_token)

        def _family_authorized(self):
            raw_cookie = self.headers.get("Cookie")
            if raw_cookie is None:
                return False
            try:
                cookie = SimpleCookie()
                cookie.load(raw_cookie)
                morsel = cookie.get("mailbox_family")
            except CookieError:
                return False
            return morsel is not None and _tokens_equal(
                morsel.value, family_cookie_value
            )

        def _unauthorized_device(self):
            self._respond(
                HTTPStatus.UNAUTHORIZED,
                b"unauthorized\n",
                "text/plain; charset=utf-8",
                extra_headers=(("WWW-Authenticate", 'Bearer realm="paper-plane-device"'),),
            )

        def _plain_error(self, status, message):
            self._respond(
                status,
                (message + "\n").encode("utf-8"),
                "text/plain; charset=utf-8",
            )

        def _respond(self, status, body, content_type, private=True, extra_headers=()):
            self.send_response(status)
            if content_type is not None:
                self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            if private:
                self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Referrer-Policy", "no-referrer")
            for name, value in extra_headers:
                self.send_header(name, value)
            self.end_headers()
            if body:
                self.wfile.write(body)

        def _render_index(self):
            messages = store.list_messages()
            cards = []
            for message in messages:
                message_id = message["id"]
                device = html.escape(message["device_id"], quote=True)
                created = html.escape(message["created_at"], quote=True)
                if message["reply_body"] is None:
                    response = """
                    <form action="/messages/{0}/reply" method="post">
                      <label for="reply-{0}">Write back</label>
                      <textarea id="reply-{0}" name="body" maxlength="2000" required></textarea>
                      <button type="submit">Send reply</button>
                    </form>""".format(message_id)
                else:
                    reply = html.escape(message["reply_body"], quote=True)
                    response = '<div class="reply"><strong>Family reply</strong><p>{}</p></div>'.format(reply)
                cards.append("""
                <article class="message-card" id="message-{0}">
                  <header><span>From {1}</span><time datetime="{2}">{2}</time></header>
                  <img src="/messages/{0}.png" alt="Drawing number {0}" loading="lazy">
                  {3}
                </article>""".format(message_id, device, created, response))
            if not cards:
                cards.append('<p class="empty">No paper planes have arrived yet.</p>')
            return """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Ian's Paper Plane Mailbox</title>
  <link rel="stylesheet" href="/static/style.css">
</head>
<body>
  <main>
    <h1>Paper Plane Mailbox</h1>
    <p class="intro">Drawings from Ian, newest first.</p>
    {0}
  </main>
</body>
</html>
""".format("\n".join(cards))

    return MailboxHTTPServer((host, port), MailboxRequestHandler)


def configuration_from_environment():
    host = os.environ.get("MAILBOX_HOST", "127.0.0.1")
    try:
        port = int(os.environ.get("MAILBOX_PORT", "8000"), 10)
    except ValueError as error:
        raise SystemExit("MAILBOX_PORT must be an integer") from error
    data_dir = Path(os.environ.get("MAILBOX_DATA_DIR", "./mailbox-data"))
    device_token = os.environ.get("MAILBOX_DEVICE_TOKEN")
    family_token = os.environ.get("MAILBOX_FAMILY_TOKEN")
    if not device_token or not family_token:
        raise SystemExit("MAILBOX_DEVICE_TOKEN and MAILBOX_FAMILY_TOKEN are required")
    return host, port, data_dir, device_token, family_token


def main():
    host, port, data_dir, device_token, family_token = configuration_from_environment()
    store = MailboxStore(data_dir)
    server = create_server(host, port, store, device_token, family_token)
    print("Paper Plane Mailbox listening on http://{}:{}".format(*server.server_address))
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
