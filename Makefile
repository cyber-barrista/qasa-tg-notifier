IMAGE ?= qasa-tg-notifier
TAG   ?= local

.PHONY: image local run debug

# Build the OCI image with nix (aarch64-linux builds go to the linux-builder
# VM) and load it into docker as $(IMAGE):$(TAG). The manifest carries a
# store-hash tag; the requested tag is applied on load.
image:
	@out=$$(nix build .#docker --no-link --print-out-paths); \
	loaded=$$(docker load < "$$out" | sed -n 's/^Loaded image: //p'); \
	docker tag "$$loaded" "$(IMAGE):$(TAG)"; \
	docker rmi "$$loaded" >/dev/null; \
	echo "==> $(IMAGE):$(TAG)"

# Alias: build the image docker-compose runs.
local:
	@$(MAKE) image TAG=local

run: local
	docker compose up

# Run the binary directly in the dev shell (no docker), loading the same .env
# that `docker compose` uses. Fast edit-build-run loop for local debugging.
debug:
	@test -f .env || { echo "no .env — copy .env.example to .env first"; exit 1; }
	@set -a; . ./.env; set +a; nix develop -c cargo run
