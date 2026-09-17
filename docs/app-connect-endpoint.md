# App Connect Endpoint — `POST /api/v1/devices/connect`

Integration guide for the **mobile / desktop apps**. One endpoint backs the
"Connect" button: it registers the device on first use and re-serves the same
profile on every later tap, returning everything the native WireGuard client
needs to bring the tunnel up.

> The server generates the WireGuard keypair for this flow. The client does
> **not** generate keys — it receives `profile.private_key` in the response
> and uses it directly. (The richer `POST /devices` is the web/manual path;
> this endpoint is the streamlined one-tap flow for apps.)

---

## 1. Authentication

The app authenticates through the **Google OAuth session flow** — there is no
password login or separate API-token system. Only accounts invited by an admin
(or the configured bootstrap admin) can sign in.

1. Open `/api/v1/auth/google/start` in the system browser and complete Google
   sign-in. An invitee must verify the emailed invitation first.
2. Complete `/api/v1/auth/google/callback` with the returned code and state;
   if prompted, complete `/api/v1/auth/google/verify-totp` as well.
3. Keep the resulting **session cookie** in the HTTP client's cookie jar and
   send it on the connect request.

All `/api/v1/devices/*` routes require a valid session; without one they
return `401`.

---

## 2. Request

```
POST /api/v1/devices/connect
Content-Type: application/json
Cookie: zerovpn_session=<session cookie from Google sign-in>
```

```jsonc
{
  // Omit on the FIRST connect. Send the value from a previous response on
  // every later connect to reuse the same device (see §5).
  "device_id": "0f9e…-uuid",

  // Optional. Display name for a newly provisioned device (e.g. the OS
  // hostname). Ignored on reconnect. 1–64 chars. Defaults to a label
  // derived from os/device_type, e.g. "macOS laptop".
  "name": "Bhadri's MacBook",

  // Optional. Auto-detect these in the app and send them.
  "os": "macos",          // ios | android | macos | windows | linux | other
  "device_type": "laptop" // phone | tablet | laptop | desktop | tv | router | watch | iot | server | other
}
```

All fields are optional. The minimal first call is `{}` (or just `os` /
`device_type`); the minimal reconnect is `{ "device_id": "…" }`.

---

## 3. Response

Both provision and reconnect return the same envelope:

```jsonc
{
  "device": {
    "id": "0f9e…-uuid",
    "name": "macOS laptop",
    "os": "macos",
    "device_type": "laptop",
    "public_key": "…",
    "allocated_ip": "10.10.0.5",
    "status": "active",
    "server_id": "…-uuid",
    "created_at": "2026-05-23T12:00:00Z"
    // …other PublicDevice fields
  },

  // Structured params — bring the tunnel up directly from these.
  "profile": {
    "private_key": "…base64…",
    "address": "10.10.0.5/32",
    "server_public_key": "…base64…",
    "endpoint": "vpn.example.com:51820",
    "allowed_ips": ["0.0.0.0/0", "::/0"],
    "mtu": 1420,
    "persistent_keepalive": 25
  },

  // Same data as a ready-to-import WireGuard .conf, plus its QR.
  "config": "[Interface]\nPrivateKey = …\n…",
  "qr_svg": "<svg …>…</svg>",

  // true  → an existing device was reused (reconnect)
  // false → a new device was provisioned
  "reused": false
}
```

### Status codes

| Code | Meaning |
|------|---------|
| `200 OK` | Existing device reconnected (`reused: true`). |
| `201 Created` | New device provisioned (`reused: false`). Also returned when the sent `device_id` was stale/revoked and the server self-healed by provisioning a fresh one. |
| `400 Bad Request` | Invalid body (e.g. `name` longer than 64 chars). |
| `401 Unauthorized` | No / expired session. Re-run login. |
| `409 Conflict` | Per-user device cap reached. |

> A stale or revoked `device_id` does **not** error — the server provisions a
> new device and returns it with `reused: false`. So the only thing to handle
> beyond the happy path is `409` (cap) and `401` (re-login).

---

## 4. Bringing up the tunnel

Map `profile` onto the native WireGuard config:

| WireGuard field | From |
|-----------------|------|
| Interface `PrivateKey` | `profile.private_key` |
| Interface `Address` | `profile.address` |
| Interface `MTU` | `profile.mtu` |
| Peer `PublicKey` | `profile.server_public_key` |
| Peer `Endpoint` | `profile.endpoint` |
| Peer `AllowedIPs` | `profile.allowed_ips` |
| Peer `PersistentKeepalive` | `profile.persistent_keepalive` |

If your platform imports a `.conf` string directly (e.g. desktop WireGuard,
`wg-quick`), use the `config` field verbatim instead.

This flow does not configure a DNS resolver. The VPN no longer exposes
VPN-managed DNS settings or hostname management.

---

## 5. The one rule: persist `device_id`

After the **first** connect, store `device.id` in the OS secure store
(Keychain / Keystore / Credential Manager). Send it back as `device_id` on
**every** later connect.

- Send it → the server reuses that one device (`reused: true`, `200`). If it's
  stale/revoked, the server transparently provisions a fresh one (`reused:
  false`, `201`) — either way you get a working profile back.
- Omit it → the server provisions a **new** device every time, which clutters
  the user's device list.

Because the server self-heals a stale id, the app logic is simply: send what
you have, then **always overwrite** the stored id with `resp.device.id` from
the response.

```text
on Connect tapped:
  id   = secureStore.get("zerovpn_device_id")          # may be null
  resp = POST /api/v1/devices/connect { device_id: id, os, device_type, name }
  secureStore.set("zerovpn_device_id", resp.device.id)  # always overwrite — id may be new
  bringUpTunnel(resp.profile)
```

No special-casing of deleted/revoked devices is needed: a stale id comes back
as a freshly provisioned device (`reused: false`).

---

## 6. Examples

### First connect (provision)

```bash
curl -sS -X POST https://vpn.example.com/api/v1/devices/connect \
  -H 'Content-Type: application/json' \
  -b cookies.txt \
  -d '{"os":"macos","device_type":"laptop","name":"My MacBook"}'
# → 201, reused:false, profile + config + qr_svg, device.id = …
```

### Reconnect

```bash
curl -sS -X POST https://vpn.example.com/api/v1/devices/connect \
  -H 'Content-Type: application/json' \
  -b cookies.txt \
  -d '{"device_id":"0f9e…-uuid"}'
# → 200, reused:true, same profile re-served
```

---

## 7. Notes & guarantees

- **Idempotent reconnect.** Reconnect re-asserts the peer on the live
  WireGuard interface, so the tunnel works even if the server's interface or
  stats worker restarted since provisioning.
- **Key storage.** The server keeps the private key KEK-encrypted so it can
  re-serve the profile on reconnect. The key only travels over TLS.
- **Auditing.** Provision logs `device.connected`; reconnect logs
  `device.reconnected` (visible in the admin/device activity timelines).
- **OpenAPI.** The endpoint is in the published spec (`GET /openapi.json`,
  served at the root — not under `/api/v1`) under the `Devices` tag —
  generate a client from there if you prefer.

Backend implementation: `connect` in
`crates/zerovpn-api/src/routes/devices.rs`.
