IMAGE ?= qasa-tg-notifier
TAG   ?= local

.PHONY: image local run

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
