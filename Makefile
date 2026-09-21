SHELL := /bin/bash
.DEFAULT_GOAL := help

# Single-file compose layout. Per-environment values (domain, TLS,
# SMTP host, WG backend, logging) live in `.env`; optional service groups
# are gated by compose profiles.
COMPOSE     := docker compose
# Dev sends real mail through the relay configured in `.env`
# (ZEROVPN_SMTP__*), same as prod. To catch mail locally instead, layer
# `-f docker-compose.mail.yml` (MailHog) back into the dev targets and point
# ZEROVPN_SMTP__HOST=mailhog / PORT=1025.
COMPOSE_DEV := $(COMPOSE) -f docker-compose.yml --profile dev
# App images (api/worker/frontend) are built locally. The overlay provides
# `build:` while the base compose names the resulting local images.
COMPOSE_BUILD := $(COMPOSE) -f docker-compose.yml -f docker-compose.build.yml
# Dev *containers*: run api/worker/web in Linux with hot-reload + a real
# (userspace) WireGuard tunnel. The overlay gates the prod api/worker/frontend/
# traefik behind the `prod` profile so only the *-dev services run.
COMPOSE_DEVCTR := $(COMPOSE) -f docker-compose.yml -f docker-compose.dev.yml --profile dev

.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage: make \033[36m<target>\033[0m\n\nTargets:\n"} /^[a-zA-Z_-]+:.*?##/ {printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

.PHONY: setup
setup: ## One-time: copy .env template + generate secrets (build happens in up/up-prod)
	@if [ ! -f .env ]; then \
		cp .env.example .env; \
		echo "Created .env from .env.example."; \
		echo ""; \
		echo "*** For production, edit .env now:"; \
		echo "***   - ZEROVPN_ENVIRONMENT=production"; \
		echo "***   - ZEROVPN_DOMAIN, ZEROVPN_PUBLIC_URL, ZEROVPN_ACME_EMAIL"; \
		echo "***   - ZEROVPN_CERT_RESOLVER=le"; \
		echo "***   - ZEROVPN_SMTP__HOST (real relay, not mailhog)"; \
		echo "***   - ZEROVPN_WG__BACKEND=kernel"; \
		echo ""; \
	fi
	@./scripts/init-secrets.sh
	@echo "Secrets ready. Run 'make up' or 'make up-prod' to build local images."

.PHONY: up
up: ## Start the dev stack locally; builds images if missing
	$(COMPOSE_BUILD) up -d

.PHONY: up-prod
up-prod: ## Build local images and start the production-shaped stack
	./scripts/deploy-prod.sh

# Local image names + git SHA for optional versioned local tags.
REGISTRY  := $(or $(shell grep -E '^ZEROVPN_REGISTRY=' .env 2>/dev/null | cut -d= -f2),local)
BASE_TAG  := $(or $(shell grep -E '^ZEROVPN_IMAGE_TAG=' .env 2>/dev/null | cut -d= -f2),latest)
GIT_SHA   := $(shell git rev-parse --short HEAD)
APP_IMAGES := crimetracker-vpn-api crimetracker-vpn-worker crimetracker-vpn-frontend

.PHONY: images
images: ## Build local app images; tags: $ZEROVPN_IMAGE_TAG + sha-<git SHA>
	$(COMPOSE_BUILD) build
	@for img in $(APP_IMAGES); do \
		docker tag $(REGISTRY)/$$img:$(BASE_TAG) $(REGISTRY)/$$img:sha-$(GIT_SHA); \
		echo "tagged $(REGISTRY)/$$img:sha-$(GIT_SHA)"; \
	done

.PHONY: down
down: ## Stop the stack
	$(COMPOSE_DEV) down

# ── Dev containers (api + worker + web in Linux, real WireGuard) ─────────────
.PHONY: up-dev
up-dev: ## Dev containers: run api/worker/web in Linux with hot-reload + real WG
	LAN_IP="$$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || echo 127.0.0.1)" \
		$(COMPOSE_DEVCTR) up -d --build
	@echo ""
	@echo "Dev containers up (first build/compile takes a few minutes — watch logs):"
	@echo "  Web (Vite)   http://localhost:6173"
	@echo "  API (debug)  http://localhost:18080"
	@echo "  WireGuard    udp/51820 on this host's LAN IP"
	@echo "  Logs:        make logs-dev"

.PHONY: down-dev
down-dev: ## Stop the dev containers
	$(COMPOSE_DEVCTR) down

.PHONY: logs-dev
logs-dev: ## Tail dev-container logs
	$(COMPOSE_DEVCTR) logs -f --tail=120

# ── Native dev loop ─────────────────────────────────────────────────────────
# Run db in docker, but run api/worker/frontend
# natively for fast iteration (cargo incremental, Vite HMR). The api +
# worker container slots are kept stopped so they don't fight for ports.

DEV_INFRA := db

.PHONY: dev
dev: ## Native dev: start infra in docker, leave api/worker/frontend for cargo + pnpm
	$(COMPOSE_DEV) up -d $(DEV_INFRA)
	$(COMPOSE_DEV) stop api worker frontend traefik 2>/dev/null || true
	@echo ""
	@echo "Infra up. In separate terminals run:"
	@echo "  make dev-api      # native zerovpn-api on http://localhost:8080"
	@echo "  make dev-worker   # native zerovpn-worker on tcp://localhost:5555"
	@echo "  make dev-web      # Vite HMR on http://localhost:6173"

.PHONY: dev-api
dev-api: ## Run the api natively against the dockerized infra
	./scripts/dev-native.sh cargo run -p zerovpn-api

.PHONY: dev-worker
dev-worker: ## Run the worker natively against the dockerized infra
	./scripts/dev-native.sh cargo run -p zerovpn-worker

.PHONY: dev-web
dev-web: ## Vite dev server with HMR (proxies /api + /ws to localhost:8080)
	cd web && pnpm dev

.PHONY: dev-migrate
dev-migrate: ## Run migrations against the dockerized db from the host
	./scripts/dev-native.sh cargo run -p zerovpn-cli -- migrate

.PHONY: dev-bootstrap-admin
dev-bootstrap-admin: ## Bootstrap admin natively. EMAIL=admin@example.com required
	@if [ -z "$(EMAIL)" ]; then echo "Usage: make dev-bootstrap-admin EMAIL=admin@example.com" && exit 1; fi
	./scripts/dev-native.sh cargo run -p zerovpn-cli -- bootstrap-admin --email $(EMAIL)

.PHONY: dev-down
dev-down: ## Stop the dev infra containers (api/worker/frontend you stop manually)
	$(COMPOSE_DEV) stop $(DEV_INFRA)

.PHONY: logs
logs: ## Tail logs
	$(COMPOSE_DEV) logs -f --tail=100

.PHONY: ps
ps: ## List containers
	$(COMPOSE_DEV) ps

.PHONY: migrate
migrate: ## Run pending migrations
	$(COMPOSE_DEV) run --rm --no-deps --entrypoint zerovpn-cli api migrate

.PHONY: bootstrap-admin
bootstrap-admin: ## Create the first admin user. EMAIL=admin@example.com required
	@if [ -z "$(EMAIL)" ]; then echo "Usage: make bootstrap-admin EMAIL=admin@example.com" && exit 1; fi
	$(COMPOSE_DEV) run --rm --no-deps --entrypoint zerovpn-cli api bootstrap-admin --email "$(EMAIL)"

.PHONY: shell-api
shell-api: ## Open a shell in the api container
	$(COMPOSE_DEV) exec api sh

.PHONY: shell-db
shell-db: ## Open a psql shell
	$(COMPOSE_DEV) exec db psql -U zerovpn -d zerovpn

.PHONY: test
test: ## Run all unit/workspace tests
	cargo test --workspace

.PHONY: test-it
test-it: ## Run DB integration tests (separate crate; needs Docker)
	cd tests && cargo test

.PHONY: smoke
smoke: ## Run end-to-end smoke test against the running stack
	./scripts/smoke-test.sh

.PHONY: check
check: ## cargo check + clippy + tsc + eslint
	cargo check --workspace --all-targets
	cargo clippy --workspace --all-targets -- -D warnings
	# tsc -b, not --noEmit: the root tsconfig is solution-style (files: [],
	# project references), so a bare `tsc --noEmit` checks nothing. -b is
	# what the frontend image build runs.
	cd web && pnpm tsc -b && pnpm lint

.PHONY: fmt
fmt: ## Format code
	cargo fmt --all
	cd web && pnpm format

.PHONY: sqlx-prepare
sqlx-prepare: ## Regenerate .sqlx offline query data
	cd crates/zerovpn-db && cargo sqlx prepare -- --tests

.PHONY: clean
clean: ## DESTRUCTIVE: stop and remove all containers, volumes, build artifacts
	@echo "This will destroy all data in the stack. Press Ctrl+C to abort, Enter to continue."
	@read _
	$(COMPOSE_DEV) down -v
	cargo clean
	rm -rf web/node_modules web/dist
