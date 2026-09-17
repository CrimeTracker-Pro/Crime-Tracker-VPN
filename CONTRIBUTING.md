# Contributing to Crime Tracker VPN

Thanks for your interest in improving Crime Tracker VPN! This guide covers how to get set up and what
we expect before a change is merged.

## Getting started

1. Fork and clone the repo.
2. Bring up the dev stack (see the [README quickstart](README.md#quickstart)):
   ```bash
   make setup
   make up-dev      # Linux containers, hot-reload, real userspace WireGuard tunnel
   ```
   Register the first account to become admin. Web is at <http://localhost:6173> and the API
   at <http://localhost:18080>. With no SMTP configured (the dev default) the API **logs**
   verification / reset links instead of sending mail — grab them from `make logs-dev`.
3. For the fastest inner loop, run the app processes natively with `make dev` +
   `make dev-api` / `make dev-worker` / `make dev-web`.

## Project layout

See [Project layout](README.md#project-layout) in the README. In short: Rust workspace under
`crates/`, the React/Vite SPA under `web/`, database migrations under `migrations/`, and docs
under `docs/`.

## Before you open a PR

Everything must pass:

```bash
make check    # cargo check + clippy (-D warnings) + tsc -b + eslint
make test     # workspace unit tests
```

> Frontend type-checking gotcha: the web root `tsconfig.json` is solution-style, so a bare
> `tsc --noEmit` checks **nothing** and exits green. Always use `pnpm tsc -b` (what
> `make check` and the image build run).

For changes that touch SQL, run `make test-it` (DB integration tests — needs Docker) and, if
you changed a query, regenerate the offline data with `make sqlx-prepare`. For dead-code
hygiene in the frontend, `cd web && pnpm dlx knip` should stay clean (config in
`web/knip.jsonc`).

### Formatting — read this

`make fmt` runs `cargo fmt --all` and `pnpm format`. **The Rust formatting config uses
nightly-only options** (`imports_granularity`, `group_imports` in `rustfmt.toml`), so you must
format with a nightly toolchain or the import grouping won't apply and CI's format check will
disagree:

```bash
rustup toolchain install nightly
cargo +nightly fmt --all
cd web && pnpm format
```

## Commit & PR conventions

- Use clear, imperative commit subjects. Conventional-commit prefixes
  (`feat:`, `fix:`, `refactor:`, `docs:`, `chore:`) are used throughout the history — please
  match them.
- Keep PRs focused; one logical change per PR is easier to review.
- Fill out the PR template: what changed, why, and how you verified it.
- Reference the issue you're closing (`Closes #123`).
- If your change alters behavior, update the relevant docs (`README.md`, `docs/`, or the
  website under `docs/index.html`) in the same PR.

## Reporting bugs & requesting features

Open an issue using the templates. For **security** issues, do **not** open a public issue —
follow [SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contributions are licensed under the project's
[AGPL-3.0-or-later](LICENSE) license.
