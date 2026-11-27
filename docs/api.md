# Classets sync API v1

The contract between the web app (`wdonayre/classets-web`) and the desktop and mobile apps
(`wdonayre/classets`). Both repos keep a copy of this file; change both together.

Base URL: `{server}/api/v1`. JSON in and out (`Content-Type: application/json`).
Times are ISO-8601 UTC strings. Sizes are bytes. Hashes are lowercase hex SHA-256.

## Authentication

### Pairing (no auth)

A device creates an ed25519 key pair, keeps the secret key on the device, and starts pairing:

`POST /pair/start`
```json
{ "name": "WILLIAM-PC", "os": "windows|macos|linux|ios|android", "app_version": "0.1.0",
  "public_key": "<base64, 32 bytes>", "kind": "desktop|mobile" }
```
`201`
```json
{ "pairing_id": "<uuid>", "user_code": "KQ7M-4TZP", "verification_url": "https://…/approve?code=KQ7M-4TZP",
  "expires_in": 600, "interval": 3 }
```

The person opens `verification_url` in a browser, signs in, and an owner or admin approves.
Meanwhile the device polls every `interval` seconds:

`POST /pair/poll` `{ "pairing_id": "<uuid>" }`

- `202` `{ "status": "pending" }`
- `200` `{ "status": "approved", "device_id": "<uuid>", "token": "<secret, shown once>",
  "user": {"id": 1, "name": "William", "email": "…"}, "workspace": {"id": 1, "name": "…"} }`
- `410` `{ "status": "expired" | "denied" | "claimed" }` (a token is handed out once only)

### Signed requests (every other endpoint)

Headers on every request:

| Header | Value |
|---|---|
| `Authorization` | `Bearer <token>` |
| `X-Classets-Device` | `<device_id>` |
| `X-Classets-Timestamp` | Unix seconds |
| `X-Classets-Signature` | base64 ed25519 signature of the string below |

String to sign (four lines joined with `\n`, no trailing newline):
```
{METHOD}
{PATH including /api/v1 and the query string, exactly as sent}
{TIMESTAMP}
{sha256 hex of the raw request body; of the empty string for GET}
```
The server refuses (`401`) a bad token, a revoked or pending device, a bad signature, or a
timestamp more than 300 s away from server time. A revoked device gets
`401 {"error":"revoked","wipe":true|false}`; with `wipe: true` it deletes its synced files.

## Endpoints

### `GET /me`
`{ "device": {"id","name","status","kind"}, "user": {...}, "workspace": {...}, "role": "owner|admin|member" }`

### `GET /folders`
Top-level and nested folders this device may sync, with the effective rules:
```json
{ "folders": [ { "id": 3, "path": "Clients/Alab Apparel", "access": "edit|view",
                 "travel": "cloud_direct|direct_only", "mode": "sync|on_demand|off" } ] }
```
Folders whose travel rule is `web_only` or `hidden`, or the person has no access to, are not listed.

### `GET /changes?since={cursor}&limit={n}`
Changes to files inside folders this device syncs (`mode` = `sync`), oldest first.
`cursor` starts at `0`. `limit` ≤ 1000 (default 500).
```json
{ "changes": [ { "id": 41, "path": "Clients/Alab Apparel/cover.jpg", "deleted": false,
                 "version_id": 88, "sha256": "…", "size": 4404019,
                 "modified_at": "2026-11-25T01:14:00Z" } ],
  "cursor": 41, "has_more": false }
```
A `deleted: true` change has the last `version_id` and no `sha256`/`size`.

### `POST /files/intent`
The device wants to upload a new or changed file.
```json
{ "path": "Clients/Alab Apparel/cover.jpg", "sha256": "…", "size": 4404019,
  "modified_at": "2026-11-25T01:14:00Z", "base_version_id": 87 }
```
`base_version_id` is the version the device last saw at that path (`null` for a new file).

- `200 {"action":"done","version_id":89}` — the server already had these bytes; nothing to upload
  (it copies them inside the bucket).
- `200 {"action":"upload","upload_id":"<uuid>","url":"…","method":"PUT","headers":{…},"expires_in":600}`
  — send the raw bytes with exactly that method and headers, then commit.
- `409 {"error":"conflict","current_version_id":90}` — someone changed the file since
  `base_version_id`. Keep both: rename the local file to
  `{name} (conflict {DEVICE} {YYYY-MM-DD}){.ext}` and upload it as a new file.
- `403 {"error":"forbidden"}` — no edit access to that folder.
- `403 {"error":"quota_full","used":…,"limit":…}` — the workspace is on built-in storage and this
  file would pass its limit. Keep the file queued and try again later (the owner can free space or
  switch to their own R2). Not 409, so it is never mistaken for a conflict.
- `422` — bad path (must sit inside a listed folder; no `..`, no leading `/`).

### `POST /files/commit` `{ "upload_id": "<uuid>" }`
`200 {"version_id": 91, "path": "…"}`. The server checks the stored object's size and
hash first; `422 {"error":"mismatch"}` if they differ (upload again).

### `GET /files/versions/{version_id}/download`
`200 {"url":"…","expires_in":600}` — a one-file link (GET the bytes from it).

### `POST /files/delete` `{ "path": "…", "version_id": 91 }`
Moves the file to Trash. `409` if `version_id` is no longer current.

### `POST /heartbeat` `{ "pending_up": 0, "pending_down": 3, "local_bytes": 123 }`
`200 {"ok": true}`. Updates "last seen" and status on the Devices page.

### Mobile companion

`GET /browse?path={folder path, empty for root}` — folders and files the person can see,
regardless of device sync rules (`hidden` excluded):
```json
{ "path": "Clients", "folders": [ {"name":"Alab Apparel","path":"Clients/Alab Apparel"} ],
  "files": [ {"name":"cover.jpg","path":"…","size":1,"version_id":88,
              "modified_at":"…","thumb_url":"…|null"} ],
  "can_upload": true }
```
`thumb_url` is a signed link valid for one hour. Folders with travel `direct_only` refuse
cloud uploads from desktops with `403 {"error":"direct_only"}` (direct sync is not built yet).
Uploads from the phone use `/files/intent` + `/files/commit` with `base_version_id: null`.

`GET /pairings/pending` (owner/admin) → `{ "pairings": [ {"user_code","name","os","kind","requested_by","created_at"} ] }`
`GET /pairings/{user_code}` (owner/admin) → the same fields for one code, so a phone can show
which device it is before approving. Works for a member's request in this workspace or a fresh
code shown on a device in front of the owner/admin; anything else is `404 not_found`.
`POST /pairings/{user_code}/approve` → `{ "device_id": "…" }` (a fresh code becomes the approver's
own device; a member's request stays theirs) · `POST /pairings/{user_code}/deny`.
