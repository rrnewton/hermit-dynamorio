SUBMODULE_PROXY ?= $(shell command -v with-proxy 2>/dev/null)
SUBMODULE_GIT = $(SUBMODULE_PROXY) git

.PHONY: checkout-all sync-submodule-policy check-identity-policy check-examples check-prototype-runtime e2e

checkout-all: sync-submodule-policy
	@$(SUBMODULE_GIT) submodule update --init --recursive

# Preserve the recursive default-checkout policy for the full DynamoRIO fork.
sync-submodule-policy:
	@$(SUBMODULE_GIT) submodule sync --recursive >/dev/null 2>&1 || true
	@git config -f .gitmodules --get-regexp '^submodule\..*\.update$$' | \
		while read -r key value; do git config "$$key" "$$value"; done

check-identity-policy:
	@./reverie-dynamorio/scripts/test-identity-policy.sh

# These are distinct C ABI producers. Do not unify them with workspace --all-features.
check-examples:
	cargo test --locked -p reverie-dynamorio-examples

check-prototype-runtime:
	cargo test --locked -p reverie-dynamorio --features prototype-runtime

e2e:
	@./reverie-dynamorio-examples/e2e/run.sh $(E2E_ARGS)

# Explicit legacy Detcore package-client producer. The link-only stub remains in
# the private build directory; only the client is emitted to the resource dir.
# The final shared-engine native producer is separate and is not qualified here.
.PHONY: package-detcore-native-client
package-detcore-native-client:
	@test -n "$(DYNAMORIO_CMAKE_DIR)" || (echo "DYNAMORIO_CMAKE_DIR is required" >&2; exit 2)
	@test -n "$(DYNAMORIO_CLIENT_BUILD_DIR)" || (echo "DYNAMORIO_CLIENT_BUILD_DIR is required" >&2; exit 2)
	@test -n "$(DYNAMORIO_RESOURCE_DIR)" || (echo "DYNAMORIO_RESOURCE_DIR is required" >&2; exit 2)
	@test -n "$(DYNAMORIO_CLIENT_JOBS)" || (echo "DYNAMORIO_CLIENT_JOBS is required" >&2; exit 2)
	cmake -S "$(CURDIR)/detcore-dynamorio/native-client" -B "$(DYNAMORIO_CLIENT_BUILD_DIR)" \
		-DCMAKE_BUILD_TYPE=Release -DDynamoRIO_DIR="$(DYNAMORIO_CMAKE_DIR)" \
		-DREVERIE_DYNAMORIO_NATIVE_SOURCE="$(CURDIR)/reverie-dynamorio/native/client.c" \
		-DHERMIT_RESOURCE_DIR="$(DYNAMORIO_RESOURCE_DIR)"
	cmake --build "$(DYNAMORIO_CLIENT_BUILD_DIR)" --config Release \
		--target reverie_dynamorio_client --parallel "$(DYNAMORIO_CLIENT_JOBS)"

# Counter1's incoming caller producer remains explicit and profile matched.
COUNTER1_CALLER_PROFILE ?= debug
.PHONY: check-counter1-caller
check-counter1-caller:
	CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=8 ./reverie-dynamorio/scripts/build-client.sh \
		--caller-manifest reverie-dynamorio-examples/counter1-dynamorio-runtime/Cargo.toml \
		--profile "$(COUNTER1_CALLER_PROFILE)" --jobs 8 --check
