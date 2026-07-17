import http.client
from pathlib import Path
import socket
import sqlite3
import struct
import tempfile
import threading
import unittest
import urllib.parse
import zlib

from server.mailbox_server import (
    MAX_IMAGE_BYTES,
    MailboxStore,
    ReplyAlreadyExists,
    ValidationError,
    create_server,
)


def png_chunk(kind, payload):
    return (
        struct.pack(">I", len(payload))
        + kind
        + payload
        + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
    )


def make_png(extra_payload=b""):
    signature = b"\x89PNG\r\n\x1a\n"
    ihdr = png_chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
    extra = png_chunk(b"tEXt", extra_payload) if extra_payload else b""
    scanline = b"\x00\xff\xff\xff"
    return signature + ihdr + extra + png_chunk(b"IDAT", zlib.compress(scanline)) + png_chunk(b"IEND", b"")


class MailboxStoreTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.data_dir = Path(self.tempdir.name)
        self.store = MailboxStore(self.data_dir)

    def tearDown(self):
        self.tempdir.cleanup()

    def test_initializes_exact_database_schema_and_enables_foreign_keys(self):
        with sqlite3.connect(self.data_dir / "mailbox.sqlite3") as db:
            tables = {
                row[0]: row[1]
                for row in db.execute(
                    "SELECT name, sql FROM sqlite_master WHERE type = 'table'"
                )
            }
            message_columns = [row[1] for row in db.execute("PRAGMA table_info(messages)")]
            reply_columns = [row[1] for row in db.execute("PRAGMA table_info(replies)")]
            foreign_keys = list(db.execute("PRAGMA foreign_key_list(replies)"))

        self.assertEqual(message_columns, ["id", "device_id", "image_path", "created_at"])
        self.assertEqual(reply_columns, ["id", "message_id", "body", "created_at"])
        self.assertIn("AUTOINCREMENT", tables["messages"].upper())
        self.assertIn("AUTOINCREMENT", tables["replies"].upper())
        self.assertEqual(foreign_keys[0][2:5], ("messages", "message_id", "id"))
        self.assertEqual(foreign_keys[0][6], "CASCADE")

    def test_create_message_persists_png_and_metadata(self):
        payload = make_png()
        message_id = self.store.create_message("ian-kobo", payload)

        self.assertEqual(message_id, 1)
        self.assertEqual((self.data_dir / "images" / "1.png").read_bytes(), payload)
        with sqlite3.connect(self.data_dir / "mailbox.sqlite3") as db:
            row = db.execute(
                "SELECT device_id, image_path, created_at FROM messages WHERE id = ?",
                (message_id,),
            ).fetchone()
        self.assertEqual(row[0:2], ("ian-kobo", "images/1.png"))
        self.assertRegex(row[2], r"^\d{4}-\d{2}-\d{2}T.*Z$")
        self.assertEqual(list((self.data_dir / "images").glob(".*.tmp")), [])

    def test_create_message_rejects_invalid_png_without_persisting(self):
        with self.assertRaisesRegex(ValidationError, "PNG"):
            self.store.create_message("ian-kobo", b"not a png")

        self.assertEqual(self.store.list_messages(), [])
        self.assertEqual(list((self.data_dir / "images").iterdir()), [])

    def test_create_message_rejects_corrupt_png_crc(self):
        payload = bytearray(make_png())
        payload[-5] ^= 0x01
        with self.assertRaisesRegex(ValidationError, "PNG"):
            self.store.create_message("ian-kobo", bytes(payload))

    def test_create_message_rejects_duplicate_malformed_ihdr(self):
        signature = b"\x89PNG\r\n\x1a\n"
        valid_ihdr = png_chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
        malformed_ihdr = png_chunk(b"IHDR", b"short")
        scanline = png_chunk(b"IDAT", zlib.compress(b"\x00\xff\xff\xff"))
        payload = signature + valid_ihdr + malformed_ihdr + scanline + png_chunk(b"IEND", b"")

        with self.assertRaisesRegex(ValidationError, "PNG"):
            self.store.create_message("ian-kobo", payload)

    def test_create_message_accepts_exact_size_limit_and_rejects_one_byte_more(self):
        base_size = len(make_png())
        exact = make_png(b"x" * (MAX_IMAGE_BYTES - base_size - 12))
        too_large = exact + b"x"

        self.assertEqual(len(exact), MAX_IMAGE_BYTES)
        self.assertEqual(self.store.create_message("ian-kobo", exact), 1)
        with self.assertRaisesRegex(ValidationError, "4 MiB"):
            self.store.create_message("ian-kobo", too_large)

    def test_create_message_rejects_empty_device_id(self):
        with self.assertRaisesRegex(ValidationError, "device"):
            self.store.create_message("", make_png())

    def test_create_reply_and_next_reply_are_ordered_by_reply_id(self):
        first_message = self.store.create_message("ian-kobo", make_png())
        second_message = self.store.create_message("ian-kobo", make_png())
        first_reply = self.store.create_reply(first_message, "First hello")
        second_reply = self.store.create_reply(second_message, "你好, Ian ✈")

        self.assertEqual(self.store.next_reply(0)["id"], first_reply)
        self.assertEqual(self.store.next_reply(first_reply), {
            "id": second_reply,
            "message_id": second_message,
            "body": "你好, Ian ✈",
        })
        self.assertIsNone(self.store.next_reply(second_reply))

    def test_create_reply_requires_existing_message_and_nonempty_body(self):
        with self.assertRaisesRegex(ValidationError, "message"):
            self.store.create_reply(999, "hello")
        message_id = self.store.create_message("ian-kobo", make_png())
        with self.assertRaisesRegex(ValidationError, "empty"):
            self.store.create_reply(message_id, "")

    def test_create_reply_enforces_character_limit_and_one_reply_per_message(self):
        message_id = self.store.create_message("ian-kobo", make_png())
        self.store.create_reply(message_id, "界" * 2000)
        with self.assertRaises(ReplyAlreadyExists):
            self.store.create_reply(message_id, "another")

        other_message = self.store.create_message("ian-kobo", make_png())
        with self.assertRaisesRegex(ValidationError, "2,000"):
            self.store.create_reply(other_message, "x" * 2001)

    def test_list_messages_returns_latest_first_with_reply(self):
        first = self.store.create_message("device <one>", make_png())
        second = self.store.create_message("device two", make_png())
        self.store.create_reply(first, "<b>hello</b>")

        messages = self.store.list_messages()

        self.assertEqual([item["id"] for item in messages], [second, first])
        self.assertIsNone(messages[0]["reply_body"])
        self.assertEqual(messages[1]["reply_body"], "<b>hello</b>")


class MailboxHTTPTests(unittest.TestCase):
    device_token = "device-secret"
    family_token = "family-secret"

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.data_dir = Path(self.tempdir.name)
        self.store = MailboxStore(self.data_dir)
        self.server = create_server(
            "127.0.0.1",
            0,
            self.store,
            device_token=self.device_token,
            family_token=self.family_token,
        )
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.tempdir.cleanup()

    def request(self, method, path, body=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        connection.request(method, path, body=body, headers=headers or {})
        response = connection.getresponse()
        response_body = response.read()
        result = response.status, dict(response.getheaders()), response_body
        connection.close()
        return result

    @property
    def device_headers(self):
        return {"Authorization": f"Bearer {self.device_token}"}

    def family_cookie(self):
        status, headers, body = self.request("GET", "/?token=" + self.family_token)
        self.assertEqual((status, body), (303, b""))
        self.assertEqual(headers["Location"], "/")
        cookie = headers["Set-Cookie"].split(";", 1)[0]
        return cookie, headers["Set-Cookie"]

    def upload(self, payload=None, device_id="ian-kobo"):
        headers = {
            **self.device_headers,
            "Content-Type": "image/png",
            "X-Device-Id": device_id,
        }
        return self.request("POST", "/api/device/messages", payload or make_png(), headers)

    def test_healthz_is_public_and_contains_no_private_data(self):
        status, headers, body = self.request("GET", "/healthz")

        self.assertEqual(status, 200)
        self.assertEqual(body, b"ok\n")
        self.assertNotIn(self.device_token.encode(), body)
        self.assertNotIn(self.family_token.encode(), body)

    def test_device_routes_require_exact_bearer_token(self):
        for authorization in (
            None,
            "Bearer wrong",
            "Bearer café",
            self.device_token,
            "bearer " + self.device_token,
        ):
            headers = {"Content-Type": "image/png", "X-Device-Id": "ian-kobo"}
            if authorization is not None:
                headers["Authorization"] = authorization
            status, response_headers, _ = self.request(
                "POST", "/api/device/messages", make_png(), headers
            )
            self.assertEqual(status, 401)
            self.assertEqual(response_headers["Cache-Control"], "no-store")

    def test_upload_requires_png_content_type_device_id_and_content_length(self):
        invalid_headers = [
            {**self.device_headers, "Content-Type": "application/octet-stream", "X-Device-Id": "ian-kobo"},
            {**self.device_headers, "Content-Type": "image/png"},
        ]
        for headers in invalid_headers:
            status, _, _ = self.request("POST", "/api/device/messages", make_png(), headers)
            self.assertEqual(status, 400 if "X-Device-Id" not in headers else 415)

        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        connection.putrequest("POST", "/api/device/messages")
        for name, value in {
            **self.device_headers,
            "Content-Type": "image/png",
            "X-Device-Id": "ian-kobo",
        }.items():
            connection.putheader(name, value)
        connection.endheaders()
        response = connection.getresponse()
        self.assertEqual(response.status, 411)
        response.read()
        connection.close()

    def test_upload_persists_raw_png_and_returns_decimal_id(self):
        payload = make_png()
        status, headers, body = self.upload(payload)

        self.assertEqual(status, 201)
        self.assertEqual(body, b"1")
        self.assertEqual(headers["Content-Type"], "text/plain; charset=utf-8")
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual((self.data_dir / "images" / "1.png").read_bytes(), payload)

    def test_upload_rejects_declared_body_over_four_mib_without_reading_it(self):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        connection.putrequest("POST", "/api/device/messages")
        for name, value in {
            **self.device_headers,
            "Content-Type": "image/png",
            "X-Device-Id": "ian-kobo",
            "Content-Length": str(MAX_IMAGE_BYTES + 1),
        }.items():
            connection.putheader(name, value)
        connection.endheaders()
        response = connection.getresponse()
        body = response.read()
        connection.close()

        self.assertEqual(response.status, 413)
        self.assertIn(b"4 MiB", body)
        self.assertEqual(self.store.list_messages(), [])

    def test_reply_poll_returns_204_then_oldest_new_utf8_reply(self):
        status, headers, body = self.request(
            "GET", "/api/device/replies?after=0", headers=self.device_headers
        )
        self.assertEqual((status, body), (204, b""))
        self.assertEqual(headers["Cache-Control"], "no-store")

        message_id = self.store.create_message("ian-kobo", make_png())
        reply_id = self.store.create_reply(message_id, "你好, Ian ✈")
        status, headers, body = self.request(
            "GET", "/api/device/replies?after=0", headers=self.device_headers
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["X-Reply-Id"], str(reply_id))
        self.assertEqual(headers["Content-Type"], "text/plain; charset=utf-8")
        self.assertEqual(body.decode("utf-8"), "你好, Ian ✈")

    def test_reply_poll_requires_one_nonnegative_decimal_after_value(self):
        for path in (
            "/api/device/replies",
            "/api/device/replies?after=-1",
            "/api/device/replies?after=x",
            "/api/device/replies?after=0&extra=1",
            "/api/device/replies?after=0&after=1",
        ):
            status, _, _ = self.request("GET", path, headers=self.device_headers)
            self.assertEqual(status, 404 if "after=" not in path else 400, path)

    def test_family_token_exchange_sets_safe_cookie_and_redirects_clean_url(self):
        cookie, set_cookie = self.family_cookie()

        self.assertIn("HttpOnly", set_cookie)
        self.assertIn("SameSite=Strict", set_cookie)
        self.assertIn("Path=/", set_cookie)
        self.assertNotIn(self.family_token, cookie)
        status, headers, _ = self.request("GET", "/", headers={"Cookie": cookie})
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")

    def test_family_pages_reject_missing_invalid_or_malformed_credentials(self):
        for path, headers in (
            ("/", {}),
            ("/?token=wrong", {}),
            ("/?token=caf%C3%A9", {}),
            ("/?token=family-secret&extra=1", {}),
            ("/", {"Cookie": "mailbox_family=wrong"}),
        ):
            status, response_headers, _ = self.request("GET", path, headers=headers)
            self.assertEqual(status, 403)
            self.assertEqual(response_headers["Cache-Control"], "no-store")

    def test_family_list_is_latest_first_responsive_and_escapes_content(self):
        first = self.store.create_message("<script>one</script>", make_png())
        second = self.store.create_message("device two", make_png())
        self.store.create_reply(first, "<script>alert(1)</script>")
        cookie, _ = self.family_cookie()

        status, _, body = self.request("GET", "/", headers={"Cookie": cookie})
        html = body.decode()

        self.assertEqual(status, 200)
        self.assertIn('<html lang="zh-CN">', html)
        self.assertIn("Ian 的纸飞机信箱", html)
        self.assertIn("给 Ian 回信", html)
        self.assertIn('<meta name="viewport" content="width=device-width, initial-scale=1">', html)
        self.assertLess(html.index(f'message-{second}'), html.index(f'message-{first}'))
        self.assertNotIn("<script>alert(1)</script>", html)
        self.assertIn("&lt;script&gt;alert(1)&lt;/script&gt;", html)
        self.assertIn("&lt;script&gt;one&lt;/script&gt;", html)
        self.assertIn(f'/messages/{second}.png', html)
        self.assertIn(f'/messages/{second}/reply', html)
        self.assertNotIn(f'/messages/{first}/reply', html)

    def test_protected_image_returns_original_png_and_static_css_is_protected(self):
        message_id = self.store.create_message("ian-kobo", make_png())
        for path in (f"/messages/{message_id}.png", "/static/style.css"):
            status, _, _ = self.request("GET", path)
            self.assertEqual(status, 403)

        cookie, _ = self.family_cookie()
        status, headers, body = self.request(
            "GET", f"/messages/{message_id}.png", headers={"Cookie": cookie}
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "image/png")
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(body, make_png())
        status, headers, body = self.request(
            "GET", "/static/style.css", headers={"Cookie": cookie}
        )
        self.assertEqual(status, 200)
        self.assertEqual(headers["Content-Type"], "text/css; charset=utf-8")
        self.assertIn(b"max-width", body)

    def test_family_can_post_utf8_reply_and_device_can_poll_it(self):
        message_id = self.store.create_message("ian-kobo", make_png())
        cookie, _ = self.family_cookie()
        encoded = urllib.parse.urlencode({"body": "加油, Ian! ✈"}).encode()

        status, headers, body = self.request(
            "POST",
            f"/messages/{message_id}/reply",
            encoded,
            {
                "Cookie": cookie,
                "Content-Type": "application/x-www-form-urlencoded; charset=utf-8",
            },
        )

        self.assertEqual((status, body), (303, b""))
        self.assertEqual(headers["Location"], f"/#message-{message_id}")
        status, headers, body = self.request(
            "GET", "/api/device/replies?after=0", headers=self.device_headers
        )
        self.assertEqual(status, 200)
        self.assertEqual(body.decode(), "加油, Ian! ✈")

    def test_reply_form_rejects_empty_too_long_duplicate_and_unknown_message(self):
        message_id = self.store.create_message("ian-kobo", make_png())
        cookie, _ = self.family_cookie()
        headers = {"Cookie": cookie, "Content-Type": "application/x-www-form-urlencoded"}

        for body, expected in (
            (urllib.parse.urlencode({"body": ""}).encode(), 400),
            (urllib.parse.urlencode({"body": "x" * 2001}).encode(), 400),
        ):
            status, _, _ = self.request(
                "POST", f"/messages/{message_id}/reply", body, headers
            )
            self.assertEqual(status, expected)

        valid = urllib.parse.urlencode({"body": "hello"}).encode()
        self.assertEqual(
            self.request("POST", f"/messages/{message_id}/reply", valid, headers)[0],
            303,
        )
        self.assertEqual(
            self.request("POST", f"/messages/{message_id}/reply", valid, headers)[0],
            409,
        )
        self.assertEqual(
            self.request("POST", "/messages/999/reply", valid, headers)[0],
            404,
        )

    def test_reply_form_rejects_transfer_encoding_and_incomplete_body(self):
        cookie, _ = self.family_cookie()

        chunked_message = self.store.create_message("ian-kobo", make_png())
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        connection.putrequest("POST", f"/messages/{chunked_message}/reply")
        connection.putheader("Cookie", cookie)
        connection.putheader("Content-Type", "application/x-www-form-urlencoded")
        connection.putheader("Content-Length", "10")
        connection.putheader("Transfer-Encoding", "chunked")
        connection.endheaders(b"body=hello")
        response = connection.getresponse()
        self.assertEqual(response.status, 400)
        self.assertEqual(response.getheader("Cache-Control"), "no-store")
        response.read()
        connection.close()
        self.assertIsNone(self.store.next_reply(0))

        incomplete_message = self.store.create_message("ian-kobo", make_png())
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        connection.putrequest("POST", f"/messages/{incomplete_message}/reply")
        connection.putheader("Cookie", cookie)
        connection.putheader("Content-Type", "application/x-www-form-urlencoded")
        connection.putheader("Content-Length", "100")
        connection.endheaders()
        connection.send(b"body=hello")
        connection.sock.shutdown(socket.SHUT_WR)
        response = connection.getresponse()
        self.assertEqual(response.status, 400)
        self.assertEqual(response.getheader("Cache-Control"), "no-store")
        response.read()
        connection.close()
        self.assertIsNone(self.store.next_reply(0))

    def test_private_routes_have_strict_matching_and_no_store_errors(self):
        cookie, _ = self.family_cookie()
        cases = (
            ("GET", "/healthz?private=1", {}),
            ("GET", "/api/device/replies/?after=0", self.device_headers),
            ("POST", "/api/device/messages/", self.device_headers),
            ("GET", "/messages/01.png", {"Cookie": cookie}),
            ("GET", "/unknown", {"Cookie": cookie}),
        )
        for method, path, headers in cases:
            status, response_headers, _ = self.request(method, path, headers=headers)
            self.assertEqual(status, 404, path)
            self.assertEqual(response_headers["Cache-Control"], "no-store")

    def test_fragments_and_unsupported_methods_are_strict_no_store_errors(self):
        for path, headers in (
            (f"/?token={self.family_token}#fragment", {}),
            ("/api/device/replies?after=0#fragment", self.device_headers),
        ):
            status, response_headers, _ = self.request("GET", path, headers=headers)
            self.assertEqual(status, 404, path)
            self.assertEqual(response_headers["Cache-Control"], "no-store")

        for method in ("HEAD", "PUT", "DELETE", "PATCH", "OPTIONS"):
            status, response_headers, _ = self.request(method, "/")
            self.assertEqual(status, 405, method)
            self.assertEqual(response_headers["Cache-Control"], "no-store")
            self.assertEqual(response_headers["Allow"], "GET, POST")


if __name__ == "__main__":
    unittest.main()
