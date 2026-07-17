# Paper Plane Mailbox server

A private, standard-library-only Python 3.11 web app for receiving PNG drawings from the Kobo and letting family members reply from a phone.

## Run

Set two different, high-entropy capability tokens and choose a persistent data directory:

```sh
export MAILBOX_DEVICE_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
export MAILBOX_FAMILY_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
export MAILBOX_DATA_DIR="$PWD/mailbox-data"
export MAILBOX_HOST="127.0.0.1"   # default
export MAILBOX_PORT="8000"        # default
python3 server/mailbox_server.py
```

Open `http://127.0.0.1:8000/?token=<family-token>` once. The server exchanges the URL token for an HttpOnly, SameSite cookie and redirects to a clean URL.

This server speaks plain HTTP. Put it behind an HTTPS reverse proxy before exposing it outside a trusted local machine. Keep both tokens secret and do not place the data directory in version control.

## Device protocol

- `POST /api/device/messages` with `Authorization: Bearer DEVICE_TOKEN`, `Content-Type: image/png`, `X-Device-Id`, and a raw PNG body no larger than 4 MiB.
- `GET /api/device/replies?after=<reply-id>` with device authorization. It returns `204` or UTF-8 text plus `X-Reply-Id`.
- `GET /healthz` is public and contains only `ok`.

## Test

```sh
python3 -m unittest -v server/test_mailbox_server.py
```

The database is `MAILBOX_DATA_DIR/mailbox.sqlite3`; original images are stored under `MAILBOX_DATA_DIR/images/`.
