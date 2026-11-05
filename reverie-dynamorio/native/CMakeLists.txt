cmake_minimum_required(VERSION 3.14)
project(reverie_dynamorio_client C)

if(NOT DEFINED DynamoRIO_DIR)
  message(FATAL_ERROR "DynamoRIO_DIR must point to Cargo's pinned full-fork install")
endif()
find_package(DynamoRIO REQUIRED)

# A core-engine Detcore client and a generic caller runtime are two explicit
# products. The core route never falls back to a monolithic Rust runtime.
if(DEFINED HERMIT_ENGINE_LIBRARY OR DEFINED HERMIT_ENGINE_NATIVE_INCLUDE_DIRECTORY OR
   DEFINED HERMIT_ENGINE_BRIDGE_SOURCE)
  if(NOT DEFINED HERMIT_ENGINE_LIBRARY OR NOT DEFINED HERMIT_ENGINE_NATIVE_INCLUDE_DIRECTORY OR
     NOT DEFINED HERMIT_ENGINE_BRIDGE_SOURCE OR DEFINED REVERIE_DYNAMORIO_RUNTIME OR
     DEFINED REVERIE_DYNAMORIO_CALLER_INPUT_IDENTITY)
    message(FATAL_ERROR "Core engine requires its exact ELF, native-header directory and bridge source, with no REVERIE_DYNAMORIO_RUNTIME")
  endif()
  foreach(core_input HERMIT_ENGINE_LIBRARY HERMIT_ENGINE_NATIVE_INCLUDE_DIRECTORY HERMIT_ENGINE_BRIDGE_SOURCE)
    if(NOT IS_ABSOLUTE "${${core_input}}" OR NOT EXISTS "${${core_input}}")
      message(FATAL_ERROR "${core_input} is not an existing absolute core-cohort input")
    endif()
  endforeach()
  if(NOT EXISTS "${HERMIT_ENGINE_NATIVE_INCLUDE_DIRECTORY}/hermit-engine-native-v1.h")
    message(FATAL_ERROR "Core engine native header is missing")
  endif()
  add_library(reverie_dynamorio_client SHARED client.c "${HERMIT_ENGINE_BRIDGE_SOURCE}")
  get_filename_component(bridge_directory "${HERMIT_ENGINE_BRIDGE_SOURCE}" DIRECTORY)
  target_include_directories(reverie_dynamorio_client PRIVATE
    "${HERMIT_ENGINE_NATIVE_INCLUDE_DIRECTORY}" "${bridge_directory}")
  target_compile_definitions(reverie_dynamorio_client PRIVATE HERMIT_DYNAMORIO_CORE_ENGINE_CLIENT=1)
  target_link_options(reverie_dynamorio_client PRIVATE -Wl,-z,defs)
  target_link_libraries(reverie_dynamorio_client "${HERMIT_ENGINE_LIBRARY}")
  get_filename_component(runtime_dir "${HERMIT_ENGINE_LIBRARY}" DIRECTORY)
else()
  # Preserve the separately selected incoming Strace/CallerArtifact product.
  if(NOT DEFINED REVERIE_DYNAMORIO_RUNTIME)
    message(FATAL_ERROR "Select the complete core-engine cohort or explicit generic caller runtime")
  endif()
  add_library(reverie_dynamorio_client SHARED client.c)
  target_link_libraries(reverie_dynamorio_client "${REVERIE_DYNAMORIO_RUNTIME}")
  get_filename_component(runtime_dir "${REVERIE_DYNAMORIO_RUNTIME}" DIRECTORY)
endif()
if(DEFINED REVERIE_DYNAMORIO_CALLER_INPUT_IDENTITY)
  string(LENGTH "${REVERIE_DYNAMORIO_CALLER_INPUT_IDENTITY}" caller_identity_length)
  if(NOT caller_identity_length EQUAL 64 OR
     NOT REVERIE_DYNAMORIO_CALLER_INPUT_IDENTITY MATCHES "^[0-9a-f]+$")
    message(FATAL_ERROR "REVERIE_DYNAMORIO_CALLER_INPUT_IDENTITY must be exactly 64 lowercase hex characters")
  endif()
  target_compile_definitions(reverie_dynamorio_client PRIVATE
    "REVERIE_DYNAMORIO_CALLER_INPUT_IDENTITY=\"${REVERIE_DYNAMORIO_CALLER_INPUT_IDENTITY}\"")
endif()
configure_DynamoRIO_client(reverie_dynamorio_client)

# DynamoRIO's private x86 loader does not implement R_X86_64_TLSDESC.
# Match Hermit's installed-client producer when GNU defaults to GNU2 TLS.
if(CMAKE_C_COMPILER_ID STREQUAL "GNU")
  target_compile_options(reverie_dynamorio_client PRIVATE -mtls-dialect=gnu)
endif()
use_DynamoRIO_extension(reverie_dynamorio_client drmgr)
use_DynamoRIO_extension(reverie_dynamorio_client drreg)
use_DynamoRIO_extension(reverie_dynamorio_client drx)
use_DynamoRIO_extension(reverie_dynamorio_client drwrap)
set_target_properties(
  reverie_dynamorio_client
  PROPERTIES SKIP_BUILD_RPATH OFF
             BUILD_RPATH "${runtime_dir}")
