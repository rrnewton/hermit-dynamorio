/* SPDX-License-Identifier: BSD-3-Clause */
/* Included only by the explicit shared-engine Detcore client product. The
 * surrounding maintained native client owns all mechanisms and sealed ABI16
 * callbacks. This file forwards facts; it owns no syscall/Detcore policy. */

_Static_assert(sizeof(controller_capture_fact_t) ==
    sizeof(struct hermit_native_controller_fact), "controller fact join");
_Static_assert(sizeof(private_memory_snapshot_t) ==
    sizeof(struct hermit_native_private_snapshot), "private snapshot join");
_Static_assert(sizeof(mapping_capture_arguments_t) ==
    sizeof(struct hermit_native_mapping_capture), "mapping capture join");
_Static_assert(offsetof(mapping_capture_arguments_t, request) ==
    offsetof(struct hermit_native_mapping_capture, request), "mapping request join");
_Static_assert(offsetof(mapping_capture_arguments_t, release_lease) ==
    offsetof(struct hermit_native_mapping_capture, release_lease), "mapping release join");

static int32_t engine_controller_capture(uintptr_t context, uint32_t operation,
    struct hermit_native_controller_fact *output) {
  if (output == NULL)
    return -EINVAL;
  controller_capture_fact_t fact = {0};
  int32_t status = runtime_callbacks_page.value.controller_capture_control(
      context, operation, &fact);
  memcpy(output, &fact, sizeof(fact));
  return status;
}

static int32_t engine_private_memory(uintptr_t context,
    const struct hermit_native_private_snapshot *expected,
    struct hermit_native_private_snapshot *output) {
  if (output == NULL)
    return -EINVAL;
  private_memory_snapshot_t expected_value, actual = {0};
  const private_memory_snapshot_t *input = NULL;
  if (expected != NULL) {
    memcpy(&expected_value, expected, sizeof(expected_value));
    input = &expected_value;
  }
  int32_t status = runtime_callbacks_page.value.runtime_private_memory(
      context, input, &actual);
  memcpy(output, &actual, sizeof(actual));
  return status;
}

static int32_t engine_mapping_capture(uintptr_t context, uint32_t operation,
    struct hermit_native_mapping_capture *arguments) {
  if (arguments == NULL)
    return -EINVAL;
  mapping_capture_arguments_t transaction;
  memcpy(&transaction, arguments, sizeof(transaction));
  int32_t status = runtime_callbacks_page.value.mapping_capture(
      context, operation, &transaction);
  memcpy(arguments, &transaction, sizeof(transaction));
  return status;
}

/* This is a new, separately versioned scalar mechanism table. It is not a
 * cast of the sealed ABI16 table and never imports its caller capabilities. */
static const hermit_native_mechanisms_v1 engine_mechanisms = {
    .version = HERMIT_ENGINE_NATIVE_ABI_V1,
    .size = sizeof(hermit_native_mechanisms_v1),
    .current = hermit_dr_engine_current_v1,
    .invoke = invoke_syscall,
    .read_registers = read_registers,
    .write_registers = write_registers,
    .compare_exchange_u32 = runtime_compare_exchange_u32,
    .controller_capture = engine_controller_capture,
    .private_memory = engine_private_memory,
    .mapping_capture = engine_mapping_capture,
    .read_memory = read_memory,
    .write_memory = write_memory,
};

static int32_t engine_current_frame(void *context,
    hermit_native_callback_v1 *output) {
  if (output == NULL || hermit_dr_engine_current_v1((uintptr_t)context) != 1)
    return -ESTALE;
  prototype_counters_t *counters =
      drmgr_get_tls_field(context, thread_state_index);
  uint64_t image = atomic_load_explicit(&image_generation, memory_order_acquire);
  if (counters == NULL || image == 0 ||
      runtime_image_descriptor.image_generation != image ||
      counters->kernel_pid != (int32_t)dr_get_process_id() ||
      counters->kernel_tid != (int32_t)dr_get_thread_id(context))
    return -ESTALE;
  int32_t parent = in_tree_parent_pid();
  *output = (hermit_native_callback_v1){
      .version = HERMIT_ENGINE_NATIVE_ABI_V1,
      .size = sizeof(*output),
      .identity = {
          .physical_pid = (int32_t)dr_get_process_id(),
          .physical_tid = (int32_t)dr_get_thread_id(context),
          /* Preserve the IDs published in the maintained scratch prefix.
           * The common Guest keeps physical procfs identity separately. */
          .tool_pid = counters->kernel_pid,
          .tool_tid = counters->kernel_tid,
          .image = image,
      },
      .context = (uintptr_t)context,
      .mechanisms = &engine_mechanisms,
      /* The maintained root sentinel is -1; the generic Guest represents
       * that absent in-tree parent as None, encoded here as zero. */
      .parent = parent > 0 ? parent : 0,
      .cpuid_interception = 1,
      .branches = atomic_load_explicit(&branch_count, memory_order_relaxed),
      /* The concrete core owner reads the application's maintained auxiliary
       * vector through read_memory/procfs. Empty C storage is not a vector. */
      .auxv = NULL,
      .auxv_count = 0,
      .initial_stack = 0,
      .signal_info = NULL,
  };
  return 0;
}

static int32_t engine_native_failed(void) {
  return atomic_load_explicit(&runtime_backend_failure, memory_order_acquire) ? 1 : 0;
}

static int32_t engine_closed_copy(void) {
  /* Same condition as the maintained post_syscall early return. The extra
   * copied_vfork_clone pre-syscall arm is retained at its original callsite. */
  return has_copied_runtime() &&
      (!runtime_uses_external_global() || is_copied_vfork_process());
}

static int32_t engine_copied_syscall(void *context, int64_t number,
    const uint64_t arguments[6], int32_t *action) {
  uint64_t image = atomic_load_explicit(&image_generation, memory_order_acquire);
  if (hermit_dr_engine_current_v1((uintptr_t)context) != 1 || image == 0 ||
      runtime_image_descriptor.image_generation != image)
    return -ESTALE;
  hermit_native_copied_callback_v1 callback = {
      .version = HERMIT_ENGINE_NATIVE_ABI_V1,
      .size = sizeof(callback),
      .physical_pid = (int32_t)dr_get_process_id(),
      .physical_tid = (int32_t)dr_get_thread_id(context),
      .image = image,
      .context = (uintptr_t)context,
      .current = hermit_dr_engine_current_v1,
  };
  /* No inherited scratch prefix supplies a Tool identity in this path. */
  return hermit_dr_engine_copied_syscall_v1(&callback, number, arguments, action);
}

/* Called before the unchanged post body clears its real birth transaction.
 * For clone3 use that transaction's fault-safely decoded original flags; do
 * not reread possibly mutated application clone_args after the kernel call.
 * The final engine frame is sampled separately AFTER the complete body and
 * its authoritative child-ID publication/canonical register writes. */
static int32_t engine_original_post_facts(void *context, int sysnum,
    hermit_native_copy_post_v1 *output) {
  if (output == NULL || hermit_dr_engine_current_v1((uintptr_t)context) != 1)
    return -ESTALE;
  prototype_counters_t *counters =
      drmgr_get_tls_field(context, thread_state_index);
  if (counters == NULL)
    return -ESTALE;
  *output = (hermit_native_copy_post_v1){
      .number = sysnum,
      .result = (int64_t)dr_syscall_get_result(context),
      .flags = counters->pending_process_clone_result != 0
          ? counters->pending_process_clone_flags
          : counters->pending_clone_flags,
  };
  return 0;
}
