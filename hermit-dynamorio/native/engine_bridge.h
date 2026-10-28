/* SPDX-License-Identifier: BSD-3-Clause */
#ifndef HERMIT_DYNAMORIO_ENGINE_BRIDGE_H
#define HERMIT_DYNAMORIO_ENGINE_BRIDGE_H

/* Must be the exact header in the held/hash-bound core artifact cohort.
 * No relative include of core source and no basename/default discovery. */
#include "hermit-engine-native-v1.h"

typedef struct {
  void *engine_thread;
  hermit_native_identity_v1 identity;
  /* Native-owner bookkeeping only. No Rust lease/pointer layout is known. */
  uint32_t deferred;
  uint32_t retired;
  /* Routes only the actual later copied-process initializer. The affine
   * original parent remains opaque and owned by Rust, not by this flag. */
  uint32_t copied_init_pending;
} hermit_dr_engine_thread_v1;

int32_t hermit_dr_engine_current_v1(uintptr_t);
int32_t hermit_dr_engine_copied_syscall_v1(const hermit_native_copied_callback_v1 *, int64_t, const uint64_t [6], int32_t *);
int32_t hermit_dr_engine_syscall_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *, int64_t, const uint64_t [6], uint64_t, hermit_native_answer_v1 *);
int32_t hermit_dr_engine_instruction_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *, uint32_t, uint32_t, uint32_t, uint64_t, hermit_native_answer_v1 *);
int32_t hermit_dr_engine_signal_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *, int32_t, uint64_t, hermit_native_answer_v1 *);
int32_t hermit_dr_engine_completed_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *);
int32_t hermit_dr_engine_thread_exit_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *, int32_t);

/* These callbacks are the actual existing client's complete post body,
 * sticky native failure observation, and current frame builder. A frame is
 * sampled again AFTER bookkeeping, not copied from a requested child ID. */
typedef void (*hermit_dr_native_post_body_v1)(void *, int);
typedef int32_t (*hermit_dr_native_failure_v1)(void);
typedef int32_t (*hermit_dr_native_frame_v1)(void *, hermit_native_callback_v1 *);
typedef int32_t (*hermit_dr_native_copy_post_facts_v1)(void *, int,
    hermit_native_copy_post_v1 *);
/* Exact maintained post-body closed-copy condition, not an engine option. */
typedef int32_t (*hermit_dr_native_closed_copy_v1)(void);
int32_t hermit_dr_engine_initialize_thread_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *);
int32_t hermit_dr_engine_thread_created_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *, const hermit_native_child_v1 *);
int32_t hermit_dr_engine_process_clone_result_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *, const hermit_native_clone_post_v1 *);
int32_t hermit_dr_engine_exec_failed_v1(hermit_dr_engine_thread_v1 *, const hermit_native_callback_v1 *);
int32_t hermit_dr_engine_post_owner_v1(hermit_dr_engine_thread_v1 *, void *, int,
    hermit_dr_native_post_body_v1, hermit_dr_native_failure_v1,
    hermit_dr_native_frame_v1, hermit_dr_native_copy_post_facts_v1,
    hermit_dr_native_closed_copy_v1);

#endif
