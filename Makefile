IMAGE ?= qasa-tg-notifier
TAG   ?= local

.PHONY: image local run debug hunt

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

# Serve the apartment-hunt results page produced by the `qasa-hunt` skill.
# python3 comes from the `skills` dev shell, so nothing is installed on the
# host and the Rust shell stays free of a Python closure.
PORT ?= 8765
hunt:
	@test -f .qasa-hunt/index.html || { echo "no .qasa-hunt/index.html - run the qasa-hunt skill first"; exit 1; }
	@echo "==> http://localhost:$(PORT)"
	@nix develop .#skills -c python3 -m http.server $(PORT) --directory .qasa-hunt
