# Security Policy

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately through **[GitHub's private vulnerability reporting](https://github.com/CrimeTracker-Pro/Crime-Tracker-VPN/security/advisories/new)**
(the "Report a vulnerability" button on the repository's Security tab). If you cannot use
that, email the maintainer at **bhadrinathan28@gmail.com** with the details.

Please include:

- A description of the issue and its impact.
- Steps to reproduce (a proof of concept if you have one).
- Affected version / commit and your environment.

You can expect an initial acknowledgement within a few days. We'll work with you on a fix and
coordinate a disclosure timeline; we're happy to credit you when the fix ships unless you'd
prefer to remain anonymous.

## Supported versions

ZeroVPN is pre-1.0 and moves fast. Security fixes land on `main`; please test against the
latest `main` before reporting.

## Scope & known limitations

ZeroVPN is deliberately transparent about its security boundaries. The following are **known,
documented design choices**, not vulnerabilities — reports about them will be closed with a
pointer here:

- **Keys are stored server-side.** The server generates and stores each device's WireGuard
  keypair; there is no zero-knowledge / client-only key mode.
- **Single key-encryption key (KEK).** The KEK is provided once via env/secret. There is no
  HSM and no automatic rotation.
- **Split-tunnel only.** Devices route the VPN subnet (`10.10.0.0/22` by default), not all
  traffic. ZeroVPN is a private-network VPN, not a full-tunnel/anonymity tool.
- **Login rate limiting is per-email**, not per-IP.
- **Google sign-in bypasses TOTP.** If you require TOTP everywhere, do not enable the Google
  link.
- **The API container runs elevated** (`NET_ADMIN` + `/dev/net/tun`) because it brings up the
  WireGuard interface in its own container (no separate `wg` sidecar).

Genuinely novel issues — auth bypass, privilege escalation, secret disclosure, injection,
SSRF, etc. — are in scope and very much wanted.

## Operator checklist

If you self-host ZeroVPN, follow the security checklist in the
[runbook](docs/runbook.md) before exposing it to the internet: strong generated secrets, a
real TLS certificate, an off-box copy of the KEK, and locked-down host firewall rules
(only `443/tcp` and `51820/udp` need to be public).
