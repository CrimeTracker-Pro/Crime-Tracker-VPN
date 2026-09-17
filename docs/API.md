# Crime Tracker VPN API

The running API serves its current OpenAPI specification at `/openapi.json`.
This page describes the access model; use that specification for request and
response fields, status codes, and the full endpoint inventory.

## Authentication and invitations

There is no self-registration, password login, password reset, or password
change API. Google OAuth is the only sign-in method. The API never creates an
account as a side effect of OAuth. The first administrator is created from
`ZEROVPN_BOOTSTRAP_ADMIN_EMAIL`; later accounts must be created by an admin.

1. An admin calls `POST /api/v1/admin/users` with the invitee's email and role.
   The account remains `pending_verification` and an invitation link is mailed.
2. The invitee opens `/invite?token=...`. The SPA sends the token to
   `POST /api/v1/auth/invitations/verify`. Links expire after 24 hours.
3. The invitee starts `GET /api/v1/auth/google/start` and completes
   `POST /api/v1/auth/google/callback` with a verified Google account using
   that same email. Only then does the pending account become active.
4. Existing active users use the same Google flow. Optional account TOTP is
   completed through `POST /api/v1/auth/google/verify-totp`.

Admins can inspect pending invitations with `GET /api/v1/admin/invitations`,
resend with `POST /api/v1/admin/invitations/{id}/resend`, and revoke with
`POST /api/v1/admin/invitations/{id}/revoke`. Revoked links cannot activate an
account. An admin can re-invite the pending email address.

Session logout is `POST /api/v1/auth/logout`; the current user is
`GET /api/v1/me`. Admin endpoints require an active admin session.

## Devices and WireGuard

`/api/v1/devices` handles device creation and listing. Device detail, edits,
pause/resume, key rotation, configuration download, quotas, ordering, and
activity are under `/api/v1/devices/{id}`. Generated WireGuard profiles do not
include a `DNS` directive. The VPN does not manage hostnames or DNS resolvers,
and no DNS configuration endpoints are available.

The API exposes bandwidth, connection history, topology, audit, and server
administration endpoints as documented in the live OpenAPI specification.
WebSocket events are served at `/api/v1/ws` for signed-in users.
