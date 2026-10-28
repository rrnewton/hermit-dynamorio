/* SPDX-License-Identifier: BSD-3-Clause */
/* Include only in the explicit common-engine Detcore product. These are the
 * maintained client-facing ABI16 exports, forwarded into a separate scalar
 * ABI1. No C record carries a Tool, Rust allocation, or lifecycle permit. */

uint32_t reverie_dynamorio_runtime_abi_version(void) {
  return 16;
}

size_t reverie_dynamorio_runtime_callbacks_size(void) {
  return sizeof(runtime_callbacks_t);
}

image_descriptor_t reverie_dynamorio_runtime_image_init(void) {
  uint64_t image = hermit_engine_native_image_begin_v1();
  /* Preserve legacy Detcore's capability-clear descriptor. In particular,
   * caller-runtime initial-thread/config/event authority is NOT imported. */
  image_descriptor_t descriptor = {0};
  descriptor.size = sizeof(descriptor);
  descriptor.abi = 16;
  descriptor.image_generation = image;
  return descriptor;
}

int32_t reverie_dynamorio_runtime_ready(uint64_t image,
    ready_snapshot_t *snapshot) {
  int32_t ready = hermit_engine_native_ready_v1(image);
  if (ready == 1 && snapshot != NULL) {
    ready_snapshot_t value = {0};
    value.size = sizeof(value);
    value.owner_generation = image;
    *snapshot = value;
  }
  /* A pending response never changes the caller's output storage. */
  return ready;
}

static int32_t engine_environment_bytes(const char *name, size_t limit,
    const uint8_t **output, size_t *length) {
  const char *value = getenv(name);
  if (value == NULL) {
    *output = NULL;
    *length = 0;
    return 0;
  }
  size_t size = strnlen(value, limit + 1);
  if (size > limit)
    return -EINVAL;
  *output = (const uint8_t *)value;
  *length = size;
  return 0;
}

int32_t reverie_dynamorio_runtime_background_init_v3(void *argument,
    uint32_t native_mode) {
  /* The original sealed callback owner, not a pointer selected by guest
   * settings. The native client itself owns this page for the whole image. */
  if (argument != &runtime_callbacks_page.value ||
      (native_mode != REVERIE_DYNAMORIO_STARTUP_STANDALONE &&
       native_mode != REVERIE_DYNAMORIO_STARTUP_COORDINATED))
    return -EINVAL;
  const runtime_callbacks_t *callbacks = argument;
  hermit_native_startup_v1 startup = {
      .version = HERMIT_ENGINE_NATIVE_ABI_V1,
      .size = sizeof(startup),
      .mode = native_mode,
      .panic_on_unsupported = callbacks->panic_on_unsupported_syscalls,
      .image = atomic_load_explicit(&image_generation, memory_order_acquire),
      .physical_owner = (int32_t)dr_get_process_id(),
      .evidence_level = callbacks->evidence_log_level,
      .diagnostic = callbacks->emit,
      .idle = callbacks->idle,
      .evidence = callbacks->emit_evidence,
  };
  /* These are the exact maintained launch variable names. Only this extension
   * knows them; the common core receives the explicit native launch facts. */
  if (engine_environment_bytes("HERMIT_DYNAMORIO_RPC_SOCKET", 107,
          &startup.control, &startup.control_len) != 0 ||
      engine_environment_bytes("HERMIT_DYNAMORIO_CONFIG_FINGERPRINT", 256,
          &startup.fingerprint, &startup.fingerprint_len) != 0 ||
      engine_environment_bytes("HERMIT_DYNAMORIO_DETCONFIG", 16 * 1024 * 1024,
          &startup.standalone_json, &startup.standalone_json_len) != 0)
    return -EINVAL;
  return hermit_engine_native_background_v1(&startup);
}
