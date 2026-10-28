/* SPDX-License-Identifier: BSD-3-Clause */
/* Transport only. No Detcore, Tool, Rust allocator, scheduler, syscall policy,
 * signal commitment, evidence framing or counter reinterpretation lives here.
 * Existing client.c is not modified or activated by this source addition. */
#include "engine_bridge.h"
#include "dr_api.h"

#include <errno.h>
#include <linux/sched.h>

/* Do not dereference a saved context or application TLS on a foreign task.
 * DynamoRIO supplies its actual current context before any old pointer is
 * used. Rust's common NativeInvocation repeats this check before each use. */
int32_t hermit_dr_engine_current_v1(uintptr_t context) {
  void *current = dr_get_current_drcontext();
  return current != NULL && (uintptr_t)current == context;
}

int32_t hermit_dr_engine_copied_syscall_v1(
    const hermit_native_copied_callback_v1 *callback, int64_t number,
    const uint64_t arguments[6], int32_t *action) {
  if (callback == NULL || action == NULL ||
      callback->version != HERMIT_ENGINE_NATIVE_ABI_V1 ||
      callback->size != sizeof(*callback) ||
      callback->current != hermit_dr_engine_current_v1 ||
      hermit_dr_engine_current_v1(callback->context) != 1 ||
      (int32_t)dr_get_process_id() != callback->physical_pid ||
      (int32_t)dr_get_thread_id((void *)callback->context) != callback->physical_tid)
    return -ESTALE;
  return hermit_engine_native_copied_syscall_v1(callback, number, arguments, action);
}

static int32_t current_frame(const hermit_native_callback_v1 *callback) {
  if (callback == NULL)
    return -EINVAL;
  if (callback->version != HERMIT_ENGINE_NATIVE_ABI_V1 ||
      callback->size != sizeof(*callback) ||
      callback->mechanisms == NULL ||
      callback->mechanisms->version != HERMIT_ENGINE_NATIVE_ABI_V1 ||
      callback->mechanisms->size != sizeof(*callback->mechanisms) ||
      callback->mechanisms->current != hermit_dr_engine_current_v1 ||
      hermit_dr_engine_current_v1(callback->context) != 1 ||
      (int32_t)dr_get_process_id() != callback->identity.physical_pid ||
      (int32_t)dr_get_thread_id((void *)callback->context) !=
          callback->identity.physical_tid)
    return -ESTALE;
  return 0;
}

static int32_t checked(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback, int permit_deferred,
    int permit_initialization) {
  if (thread == NULL || thread->engine_thread == NULL || thread->retired ||
      (!permit_deferred && thread->deferred) ||
      (!permit_initialization && thread->copied_init_pending))
    return -EPROTO;
  int32_t status = current_frame(callback);
  if (status != 0)
    return status;
  if (callback->identity.physical_pid != thread->identity.physical_pid ||
      callback->identity.physical_tid != thread->identity.physical_tid ||
      callback->identity.tool_pid != thread->identity.tool_pid ||
      callback->identity.tool_tid != thread->identity.tool_tid ||
      callback->identity.image != thread->identity.image)
    return -ESTALE;
  return 0;
}

static int32_t answered(hermit_dr_engine_thread_v1 *thread, int32_t status,
    const hermit_native_answer_v1 *answer, int permit_original) {
  if (status != 0)
    return status;
  if (answer->reserved != 0 || answer->action < 0 ||
      answer->action > (permit_original ? 3 : 2))
    return -EPROTO;
  if (answer->action == 2 || answer->action == 3)
    thread->deferred = 1;
  return 0;
}

int32_t hermit_dr_engine_syscall_v1(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback, int64_t number,
    const uint64_t arguments[6], uint64_t ip, hermit_native_answer_v1 *answer) {
  int32_t status = checked(thread, callback, 0, 0);
  if (status != 0 || arguments == NULL || answer == NULL)
    return status != 0 ? status : -EINVAL;
  status = hermit_engine_native_syscall_v1(thread->engine_thread, callback,
      number, (const uint64_t (*)[6])arguments, ip, answer);
  return answered(thread, status, answer, 1);
}

int32_t hermit_dr_engine_instruction_v1(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback, uint32_t kind, uint32_t leaf,
    uint32_t subleaf, uint64_t ip, hermit_native_answer_v1 *answer) {
  int32_t status = checked(thread, callback, 0, 0);
  if (status != 0 || answer == NULL)
    return status != 0 ? status : -EINVAL;
  status = hermit_engine_native_instruction_v1(thread->engine_thread, callback,
      kind, leaf, subleaf, ip, answer);
  return answered(thread, status, answer, 0);
}

int32_t hermit_dr_engine_signal_v1(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback, int32_t signal, uint64_t ip,
    hermit_native_answer_v1 *answer) {
  int32_t status = checked(thread, callback, 0, 0);
  if (status != 0 || answer == NULL)
    return status != 0 ? status : -EINVAL;
  status = hermit_engine_native_signal_v1(thread->engine_thread, callback,
      signal, ip, answer);
  return answered(thread, status, answer, 0);
}

int32_t hermit_dr_engine_completed_v1(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback) {
  int32_t status = checked(thread, callback, 1, 0);
  if (status != 0 || !thread->deferred)
    return status != 0 ? status : -EPROTO;
  status = hermit_engine_native_completed_v1(thread->engine_thread, callback);
  if (status == 0)
    thread->deferred = 0;
  return status;
}

int32_t hermit_dr_engine_thread_exit_v1(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback, int32_t exit_code) {
  if (thread == NULL)
    return -EINVAL;
  /* The maintained capability-clear ABI returns0 for uninitialized state,
   * with no Tool invocation or explicit terminal receipt. */
  if (thread->engine_thread == NULL)
    return 0;
  int32_t status = checked(thread, callback, 1, 0);
  if (status != 0)
    return status;
  void *original = thread->engine_thread;
  /* Clear the actual native locator before its consuming Rust owner. No
   * returned Tool Result can authorize retry, a fallback or another engine. */
  thread->engine_thread = NULL;
  thread->retired = 1;
  thread->deferred = 0;
  return hermit_engine_native_thread_exit_v1(original, callback, exit_code);
}

int32_t hermit_dr_engine_initialize_thread_v1(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback) {
  if (thread == NULL || thread->retired || thread->deferred)
    return -ESTALE;
  int32_t status = current_frame(callback);
  if (status != 0)
    return status;
  if (thread->engine_thread != NULL) {
    status = checked(thread, callback, 0, 1);
    if (status != 0 || !thread->copied_init_pending)
      return status != 0 ? status : -ESTALE;
    /* Called only from the maintained first-subsequent-event copied-process
     * initializer. A post-completed owner cannot be used as an ordinary call. */
    status = hermit_engine_native_initialize_copied_thread_v1(
        thread->engine_thread, callback);
    if (status == 0)
      thread->copied_init_pending = 0;
    else
      thread->retired = 1;
    return status;
  }
  if (thread->copied_init_pending)
    return -ESTALE;
  void *created = NULL;
  status = hermit_engine_native_initialize_thread_v1(callback, &created);
  /* A retry leaves this native locator unpublished; do not construct an empty
   * parent/default state or return a locator before start/post-exec complete. */
  if (status == 1)
    return created == NULL ? 1 : -EPROTO;
  if (status != 0)
    return status;
  if (created == NULL)
    return -EPROTO;
  thread->identity = callback->identity;
  thread->engine_thread = created;
  return 0;
}

int32_t hermit_dr_engine_thread_created_v1(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback, const hermit_native_child_v1 *child) {
  int32_t status = checked(thread, callback, 1, 0);
  if (status != 0 || !thread->deferred || child == NULL)
    return status != 0 ? status : -EPROTO;
  status = hermit_engine_native_thread_created_v1(thread->engine_thread, callback, child);
  if (status < 0)
    thread->retired = 1;
  return status;
}

int32_t hermit_dr_engine_post_owner_v1(hermit_dr_engine_thread_v1 *thread,
    void *context, int sysnum, hermit_dr_native_post_body_v1 body,
    hermit_dr_native_failure_v1 failed, hermit_dr_native_frame_v1 frame,
    hermit_dr_native_copy_post_facts_v1 sample_post,
    hermit_dr_native_closed_copy_v1 closed_copy) {
  if (thread == NULL || body == NULL || failed == NULL || frame == NULL ||
      sample_post == NULL || closed_copy == NULL ||
      thread->engine_thread == NULL || thread->retired ||
      hermit_dr_engine_current_v1((uintptr_t)context) != 1)
    return -ESTALE;
  /* The native birth body consumes/clears its flags and can normalize the
   * return. Preserve its ORIGINAL kernel facts before running it once. */
  hermit_native_copy_post_v1 facts = {0};
  int32_t status = sample_post(context, sysnum, &facts);
  if (status != 0 || facts.number != sysnum)
    return status != 0 ? status : -ESTALE;
  /* A read-only binding to the authentic incoming C locator. It is not a
   * second Rust owner. The maintained body resets birth/evidence fields and
   * kernel IDs, never this appended field or the original affine lease. */
  const hermit_dr_engine_thread_v1 original = *thread;
  /* The unchanged maintained body includes ALL early returns, clone birth,
   * descriptor/evidence/register work and failed-exec bookkeeping. A return
   * from thread_created within that body does not complete Rust scratch. */
  body(context, sysnum);
  if (failed() != 0) {
    if ((int32_t)dr_get_process_id() == thread->identity.physical_pid)
      thread->retired = 1;
    return -EPROTO;
  }
  if (thread->engine_thread != original.engine_thread ||
      thread->identity.physical_pid != original.identity.physical_pid ||
      thread->identity.physical_tid != original.identity.physical_tid ||
      thread->identity.tool_pid != original.identity.tool_pid ||
      thread->identity.tool_tid != original.identity.tool_tid ||
      thread->identity.image != original.identity.image ||
      thread->deferred != original.deferred ||
      thread->retired != original.retired ||
      thread->copied_init_pending != original.copied_init_pending)
    return -ESTALE;
  int32_t closed = closed_copy();
  if (closed != 0 && closed != 1)
    return -EPROTO;
  /* Preserve the actual post_syscall early return for standalone copied
   * children and shared-VM/vfork children. In a vfork child these fields are
   * the suspended parent's storage: no completion, COW rebind or mutation. */
  if (closed == 1)
    return 0;
  if ((int32_t)dr_get_process_id() != thread->identity.physical_pid) {
    /* The actual delivery matrix has no shared-VM/vfork child post. Refuse
     * an unexpected one without consuming/mutating its shared parent owner.
     * Only an independent child can replace a copied engine locator. */
    if (facts.result != 0 || (facts.flags & (CLONE_VM | CLONE_VFORK)) != 0 ||
        !thread->deferred || thread->copied_init_pending)
      return -ESTALE;
    hermit_native_callback_v1 child = {0};
    status = frame(context, &child);
    if (status != 0 || current_frame(&child) != 0)
      return status != 0 ? status : -ESTALE;
    void *created = NULL;
    status = hermit_engine_native_copied_post_v1(thread->engine_thread,
        &child, &facts, &created);
    if (status != 0) {
      thread->retired = 1;
      return status;
    }
    if (created == NULL) {
      thread->retired = 1;
      return -EPROTO;
    }
    thread->engine_thread = created;
    thread->identity = child.identity;
    thread->deferred = 0;
    thread->copied_init_pending = 1;
    return 0;
  }
  if (!thread->deferred)
    return 0;
  hermit_native_callback_v1 current = {0};
  status = frame(context, &current);
  if (status != 0)
    return status;
  /* Parent completion remains after the whole body and canonical registers.
   * The COW branch above consumed only its own copied original continuation. */
  return hermit_dr_engine_completed_v1(thread, &current);
}

int32_t hermit_dr_engine_process_clone_result_v1(
    hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback,
    const hermit_native_clone_post_v1 *facts) {
  int32_t status = checked(thread, callback, 1, 0);
  if (status != 0 || !thread->deferred || facts == NULL)
    return status != 0 ? status : -EPROTO;
  return hermit_engine_native_process_clone_result_v1(
      thread->engine_thread, callback, facts);
}

int32_t hermit_dr_engine_exec_failed_v1(hermit_dr_engine_thread_v1 *thread,
    const hermit_native_callback_v1 *callback) {
  if (thread == NULL)
    return -EINVAL;
  if (thread->engine_thread == NULL)
    return hermit_engine_native_exec_failed_v1(NULL, callback);
  int32_t status = checked(thread, callback, 1, 0);
  if (status != 0 || !thread->deferred)
    return status != 0 ? status : -EPROTO;
  return hermit_engine_native_exec_failed_v1(thread->engine_thread, callback);
}
