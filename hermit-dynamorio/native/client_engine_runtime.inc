/* SPDX-License-Identifier: BSD-3-Clause */
/* Selected only by HERMIT_DYNAMORIO_CORE_ENGINE_CLIENT. These adapters
 * implement the existing native transport signatures with the ONE core
 * engine. All application/FP/register/evidence/DR ownership stays in client.c.
 * No Rust runtime object, Tool, scheduler or syscall policy is duplicated. */

static int32_t engine_callback_frame(prototype_counters_t *counters,
    void *context, int32_t tid, int32_t pid, uint64_t branches,
    syscall_invoker_t invoke, register_reader_t read,
    register_writer_t write, hermit_native_callback_v1 *frame) {
  if (context == NULL || counters == NULL ||
      drmgr_get_tls_field(context, thread_state_index) != counters ||
      tid != (int32_t)dr_get_thread_id(context) ||
      pid != (int32_t)dr_get_process_id() || invoke != invoke_syscall ||
      read != read_registers || write != write_registers)
    return -ESTALE;
  int32_t status = engine_current_frame(context, frame);
  if (status == 0)
    frame->branches = branches;
  return status;
}

static int32_t engine_callback_ip(void *context, uint64_t *ip) {
  struct user_regs_struct registers;
  if (read_registers((uintptr_t)context, &registers) == 0)
    return -EIO;
  *ip = registers.rip;
  return 0;
}

/* Existing void/ignored-status entry points must not swallow engine identity,
 * RPC, retirement or tail failures. Their original native fatal owner closes
 * the actual process tree; this is never a raw-kernel/runtime fallback. */
static void engine_require_status(int32_t status) {
  if (status == 0)
    return;
  atomic_store_explicit(&runtime_backend_failure, true, memory_order_release);
  dr_fprintf(diagnostic_file,
      "reverie-dynamorio: common engine mechanism refused (%d)\n", status);
  exit_runtime_tree(CLIENT_THREAD_START_FAILURE_EXIT_CODE);
}

uint32_t hermit_dr_client_abi_version(void) {
  return REVERIE_DYNAMORIO_RUNTIME_ABI_VERSION;
}
size_t hermit_dr_client_callbacks_size(void) { return sizeof(runtime_callbacks_t); }

image_descriptor_t hermit_dr_client_image_init(void) {
  /* Capability-clear legacy Detcore, not the caller runtime's capabilities
   * or input identity. The real native image owner validates this record. */
  image_descriptor_t result = {0};
  result.size = sizeof(result);
  result.abi = REVERIE_DYNAMORIO_RUNTIME_ABI_VERSION;
  result.image_generation = hermit_engine_native_image_begin_v1();
  return result;
}

int32_t hermit_dr_client_ready(uint64_t generation, ready_snapshot_t *snapshot) {
  int32_t ready = hermit_engine_native_ready_v1(generation);
  if (ready == 1 && snapshot != NULL) {
    ready_snapshot_t value = {0};
    value.size = sizeof(value);
    value.owner_generation = generation;
    *snapshot = value;
  }
  return ready;
}

static void engine_diagnostic(const uint8_t *bytes, size_t length) {
  runtime_callbacks_page.value.emit((const char *)bytes, length);
}
static void engine_evidence(const uint8_t *bytes, size_t length) {
  runtime_callbacks_page.value.emit_evidence((const char *)bytes, length);
}

int32_t hermit_dr_client_background_init_v3(void *argument, uint32_t mode) {
  const runtime_callbacks_t *callbacks = argument;
  if (callbacks != &runtime_callbacks_page.value ||
      callbacks->emit == NULL || callbacks->emit_evidence == NULL ||
      callbacks->idle == NULL || runtime_process_control == NULL ||
      runtime_owner_pid <= 0 ||
      runtime_process_control->helper_pid != (int32_t)getpid() ||
      (mode != REVERIE_DYNAMORIO_STARTUP_STANDALONE &&
       mode != REVERIE_DYNAMORIO_STARTUP_COORDINATED))
    return -ESTALE;
  const char *control = getenv("HERMIT_DYNAMORIO_RPC_SOCKET");
  const char *fingerprint = getenv("HERMIT_DYNAMORIO_CONFIG_FINGERPRINT");
  const char *json = getenv("HERMIT_DYNAMORIO_DETCONFIG");
  hermit_native_startup_v1 startup = {
      .version = HERMIT_ENGINE_NATIVE_ABI_V1,
      .size = sizeof(startup),
      .mode = mode,
      .panic_on_unsupported = callbacks->panic_on_unsupported_syscalls,
      .image = atomic_load_explicit(&image_generation, memory_order_acquire),
      .physical_owner = runtime_owner_pid,
      .evidence_level = callbacks->evidence_log_level,
      .background_pid = runtime_process_control->helper_pid,
      .reserved = 0,
      .diagnostic = engine_diagnostic,
      .idle = callbacks->idle,
      .evidence = engine_evidence,
      .control = (const uint8_t *)control,
      .control_len = control == NULL ? 0 : strlen(control),
      .fingerprint = (const uint8_t *)fingerprint,
      .fingerprint_len = fingerprint == NULL ? 0 : strlen(fingerprint),
      .standalone_json = (const uint8_t *)json,
      .standalone_json_len = json == NULL ? 0 : strlen(json),
  };
  return hermit_engine_native_background_v1(&startup);
}

void hermit_dr_client_process_exit(void) { hermit_engine_native_process_exit_v1(); }
const char *hermit_dr_client_name(void) { return "Detcore"; }
uint8_t hermit_dr_client_kind_code(void) { return UINT8_MAX; }
void hermit_dr_client_totals(uint64_t *branches, uint64_t *syscalls,
    uint64_t *rewritten, uint64_t *memory_hash) {
  engine_require_status(hermit_engine_native_totals_v1(
      branches, syscalls, rewritten, memory_hash));
}

int32_t hermit_dr_client_thread_init(prototype_counters_t *counters,
    void *context, int32_t tid, int32_t pid, int32_t parent, uint64_t branches,
    int32_t defer, syscall_invoker_t invoke, register_reader_t read,
    register_writer_t write) {
  if (counters == NULL || context == NULL ||
      context != dr_get_current_drcontext() ||
      drmgr_get_tls_field(context, thread_state_index) != counters ||
      tid != (int32_t)dr_get_thread_id(context) ||
      pid != (int32_t)dr_get_process_id() || parent != in_tree_parent_pid() ||
      invoke != invoke_syscall || read != read_registers || write != write_registers)
    return -ESTALE;
  if (defer == 1) {
    /* The actual thread-init owner has just zeroed this appended locator and
     * published its primitive birth prefix. Never construct a Tool here. */
    return counters->common_engine.engine_thread == NULL &&
        counters->common_engine.deferred == 0 &&
        counters->common_engine.retired == 0 &&
        counters->common_engine.copied_init_pending == 0 ? 0 : -ESTALE;
  }
  if (defer != 0)
    return -EINVAL;
  uint64_t generation = atomic_load_explicit(&image_generation, memory_order_acquire);
  if (hermit_engine_native_ready_v1(generation) != 1)
    return 1;
  hermit_native_callback_v1 frame;
  int32_t status = engine_callback_frame(counters, context, tid, pid, branches,
      invoke, read, write, &frame);
  if (status != 0)
    return status;
  return hermit_dr_engine_initialize_thread_v1(&counters->common_engine, &frame);
}

int32_t hermit_dr_client_thread_created_v2(prototype_counters_t *counters,
    void *context, int32_t parent_tid, int32_t pid, uint64_t branches,
    int32_t child_tid, int32_t child_runtime_tid, uint64_t child_tid_address,
    uint64_t flags, syscall_invoker_t invoke, register_reader_t read,
    register_writer_t write) {
  hermit_native_callback_v1 frame;
  int32_t status = engine_callback_frame(counters, context, parent_tid, pid,
      branches, invoke, read, write, &frame);
  if (status != 0)
    return status;
  hermit_native_child_v1 child = {
      .identity = {.physical_pid = pid, .physical_tid = child_tid,
          .tool_pid = frame.identity.tool_pid, .tool_tid = child_runtime_tid,
          .image = frame.identity.image},
      .child_tid_address = child_tid_address, .flags = flags,
      .exit_signal = (int32_t)(flags & UINT64_C(0xff)), .reserved = 0,
  };
  return hermit_dr_engine_thread_created_v1(&counters->common_engine, &frame, &child);
}

int32_t hermit_dr_client_process_clone_result(prototype_counters_t *counters,
    void *context, int32_t parent_tid, int32_t parent_pid, uint64_t branches,
    int64_t number, int64_t result, int32_t child_tid,
    uint64_t child_tid_address, uint64_t flags, int32_t exit_signal,
    syscall_invoker_t invoke, register_reader_t read, register_writer_t write) {
  if (counters == NULL || context != dr_get_current_drcontext() ||
      invoke != invoke_syscall || read != read_registers || write != write_registers)
    return -ESTALE;
  /* Real independent-child post belongs to the original copied continuation.
   * Its full native post body runs before copied_post; no inherited registry
   * or parent's deferred state is entered through this parent-only callback. */
  if (result == 0 && has_copied_runtime() &&
      parent_pid != counters->common_engine.identity.physical_pid)
    return 0;
  hermit_native_callback_v1 frame;
  int32_t status = engine_callback_frame(counters, context, parent_tid, parent_pid,
      branches, invoke, read, write, &frame);
  if (status != 0)
    return status;
  hermit_native_clone_post_v1 facts = {.number = number, .result = result,
      .child_tid_address = child_tid_address, .flags = flags,
      .tool_child_tid = child_tid, .exit_signal = exit_signal};
  return hermit_dr_engine_process_clone_result_v1(&counters->common_engine, &frame, &facts);
}

void hermit_dr_client_exec_failed(prototype_counters_t *counters, int32_t pid) {
  void *context = dr_get_current_drcontext();
  hermit_native_callback_v1 frame;
  int32_t status = engine_current_frame(context, &frame);
  if (status == 0 && (counters != drmgr_get_tls_field(context, thread_state_index) ||
      pid != frame.identity.physical_pid))
    status = -ESTALE;
  if (status == 0)
    status = hermit_dr_engine_exec_failed_v1(&counters->common_engine, &frame);
  engine_require_status(status);
}

int32_t hermit_dr_client_copied_syscall(int64_t number, const uint64_t *arguments) {
  int32_t action = 0;
  int32_t status = engine_copied_syscall(dr_get_current_drcontext(), number, arguments, &action);
  engine_require_status(status);
  return status == 0 ? action : -1;
}

int32_t hermit_dr_client_pre_syscall(void *context, prototype_counters_t *counters,
    int32_t tid, int32_t pid, uint64_t generation, int64_t number,
    const uint64_t *arguments, uint64_t branches, int64_t *result,
    int64_t *deferred_number, uint64_t *deferred_arguments,
    syscall_invoker_t invoke, register_reader_t read, register_writer_t write,
    memory_reader_t memory_read, memory_writer_t memory_write, reverie_emit_fn_t emit) {
  if (arguments == NULL || result == NULL || deferred_number == NULL ||
      deferred_arguments == NULL || memory_read != read_memory ||
      memory_write != write_memory || emit != runtime_callbacks_page.value.emit)
    return -EINVAL;
  hermit_native_callback_v1 frame;
  int32_t status = engine_callback_frame(counters, context, tid, pid, branches,
      invoke, read, write, &frame);
  if (status != 0 || frame.identity.image != generation)
    return status != 0 ? status : -ESTALE;
  uint64_t ip;
  status = engine_callback_ip(context, &ip);
  hermit_native_answer_v1 answer = {0};
  if (status == 0)
    status = hermit_dr_engine_syscall_v1(&counters->common_engine, &frame,
        number, arguments, ip, &answer);
  if (status != 0)
    return status;
  if (answer.action == 1) { *result = answer.value; return 1; }
  if (answer.action == 2) {
    *deferred_number = answer.number;
    memcpy(deferred_arguments, answer.arguments, sizeof(answer.arguments));
    return 2;
  }
  if (answer.action == 3 && answer.number == number &&
      memcmp(answer.arguments, arguments, sizeof(answer.arguments)) == 0)
    return 0; /* Original native lifecycle path, NOT action2 rewriting. */
  return -EPROTO;
}

int32_t hermit_dr_client_signal_event(void *context, void *opaque,
    int32_t tid, int32_t pid, uint64_t generation, uint64_t branches,
    dynamorio_signal_event_t *event, syscall_invoker_t invoke,
    register_reader_t read, register_writer_t write, memory_reader_t memory_read,
    memory_writer_t memory_write, reverie_emit_fn_t emit) {
  prototype_counters_t *counters = opaque;
  if (event == NULL || event->reserved != 0 || memory_read != read_memory ||
      memory_write != write_memory || emit != runtime_callbacks_page.value.emit)
    return -EINVAL;
  hermit_native_callback_v1 frame;
  int32_t status = engine_callback_frame(counters, context, tid, pid, branches,
      invoke, read, write, &frame);
  if (status != 0 || frame.identity.image != generation)
    return status != 0 ? status : -ESTALE;
  frame.signal_info = (const uint8_t (*)[128])&event->signal_info;
  uint64_t ip;
  status = engine_callback_ip(context, &ip);
  hermit_native_answer_v1 answer = {0};
  if (status == 0)
    status = hermit_dr_engine_signal_v1(&counters->common_engine, &frame,
        event->signal, ip, &answer);
  if (status != 0)
    return status;
  if (answer.action == 0) {
    event->delivered_signal = answer.signal;
    event->signal_info_changed = (int32_t)answer.signal_info_changed;
    memcpy(event->signal_info, answer.signal_info, sizeof(event->signal_info));
  } else if (answer.action == 2) {
    event->deferred_sysnum = answer.number;
    memcpy(event->deferred_args, answer.arguments, sizeof(event->deferred_args));
  }
  return answer.action;
}

int32_t hermit_dr_client_rdtsc(void *context, prototype_counters_t *counters,
    int32_t tid, int32_t pid, uint64_t branches, int32_t with_aux,
    syscall_invoker_t invoke, register_reader_t read, register_writer_t write,
    reverie_emit_fn_t emit, uint64_t *tsc, uint32_t *aux) {
  if (tsc == NULL || aux == NULL || (with_aux != 0 && with_aux != 1) ||
      emit != runtime_callbacks_page.value.emit)
    return -EINVAL;
  hermit_native_callback_v1 frame;
  int32_t status = engine_callback_frame(counters, context, tid, pid, branches,
      invoke, read, write, &frame);
  uint64_t ip;
  if (status == 0) status = engine_callback_ip(context, &ip);
  hermit_native_answer_v1 answer = {0};
  if (status == 0) status = hermit_dr_engine_instruction_v1(&counters->common_engine,
      &frame, with_aux ? 2 : 1, 0, 0, ip, &answer);
  if (status != 0 || answer.action != 1)
    return status != 0 ? status : -EPROTO;
  *tsc = (uint64_t)answer.value; *aux = answer.registers[0]; return 1;
}

int32_t hermit_dr_client_cpuid(void *context, prototype_counters_t *counters,
    int32_t tid, int32_t pid, uint64_t branches, uint32_t leaf, uint32_t subleaf,
    syscall_invoker_t invoke, register_reader_t read, register_writer_t write,
    reverie_emit_fn_t emit, uint32_t registers[4]) {
  if (registers == NULL || emit != runtime_callbacks_page.value.emit)
    return -EINVAL;
  hermit_native_callback_v1 frame;
  int32_t status = engine_callback_frame(counters, context, tid, pid, branches,
      invoke, read, write, &frame);
  uint64_t ip;
  if (status == 0) status = engine_callback_ip(context, &ip);
  hermit_native_answer_v1 answer = {0};
  if (status == 0) status = hermit_dr_engine_instruction_v1(&counters->common_engine,
      &frame, 0, leaf, subleaf, ip, &answer);
  if (status != 0 || answer.action != 1)
    return status != 0 ? status : -EPROTO;
  memcpy(registers, answer.registers, sizeof(answer.registers)); return 1;
}

int32_t hermit_dr_client_thread_exit(prototype_counters_t *counters,
    void *context, int32_t tid, syscall_invoker_t invoke, const terminal_fact_t *fact) {
  /* Capability-clear Detcore never receives a fabricated terminal fact. */
  if (fact != NULL || counters == NULL || invoke != invoke_syscall) {
    engine_require_status(-ESTALE);
    return -ESTALE;
  }
  if (counters->common_engine.engine_thread == NULL)
    return 0; /* Maintained uninitialized/no-Tool exit contract. */
  hermit_native_callback_v1 frame;
  int32_t status = engine_current_frame(context, &frame);
  if (status == 0 && (tid != frame.identity.physical_tid ||
      drmgr_get_tls_field(context, thread_state_index) != counters))
    status = -ESTALE;
  if (status == 0)
    status = hermit_dr_engine_thread_exit_v1(&counters->common_engine, &frame, 0);
  engine_require_status(status);
  return status;
}
