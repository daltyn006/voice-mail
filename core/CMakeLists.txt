cmake_minimum_required(VERSION 3.24)

add_library(present_core SHARED
    src/dllmain.cpp
    src/pipeline.cpp
    src/audio_ffmpeg.cpp
    src/audio_audacity.cpp
    src/srt.cpp
    src/stt_whisper.cpp
    src/llm_llama.cpp
    src/vlm_qwen.cpp
    src/validate.cpp
    src/agent_guide.cpp
    src/diff.cpp
    src/meta_day_class.cpp
    src/db.cpp
    src/vram_guard.cpp
    src/hw_detect.cpp
)

target_include_directories(present_core PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/include)
target_compile_definitions(present_core PRIVATE PRESENT_CORE_BUILD NOMINMAX WIN32_LEAN_AND_MEAN)

# GPU backend selection. Vendor arrives from build.ps1 (-DPV_GPU_VENDOR,
# itself from Win32_VideoController + nvidia-smi, env-overridable for
# foreign-system builds); AUTO means "try Vulkan, SDK presence decides".
# NONE (Intel-only/headless/unknown) never touches Vulkan: no SDK lookup,
# no REQUIRED find, CPU-only, always configures.
# A missing SDK or dead driver can never abort the pipeline: GGML_VULKAN /
# LLAMA_VULKAN are forced OFF unconditionally below, so stale -D flags or a
# reused core/build cache die here instead of inside ggml-vulkan's
# REQUIRED find_package (the observed fatal error).
set(PV_GPU_VENDOR "AUTO" CACHE STRING "GPU vendor identity: AUTO|NVIDIA|AMD|NONE")
set(_PV_TRY_VULKAN ${PV_ENABLE_VULKAN})
if(PV_GPU_VENDOR STREQUAL "NONE")
  set(_PV_TRY_VULKAN OFF)
  message(STATUS "present_core: vendor NONE (Intel-only/headless) -> CPU-only backends")
elseif(PV_GPU_VENDOR STREQUAL "NVIDIA")
  find_program(_PV_SMI nvidia-smi)
  if(_PV_SMI)
    execute_process(COMMAND ${_PV_SMI} -L RESULT_VARIABLE _PV_SMI_RC OUTPUT_QUIET ERROR_QUIET)
  endif()
  if(NOT _PV_SMI OR NOT _PV_SMI_RC EQUAL 0)
    set(_PV_TRY_VULKAN OFF)
    message(STATUS "present_core: nvidia-smi unhealthy/missing -> CPU-only backends")
  endif()
endif()
set(_PV_VULKAN OFF)
if(_PV_TRY_VULKAN)
  find_package(Vulkan QUIET)
  if(Vulkan_FOUND)
    set(_PV_VULKAN ON)
    message(STATUS "present_core: Vulkan SDK found -> GPU backends")
  else()
    message(STATUS "present_core: no Vulkan SDK -> CPU-only backends")
  endif()
endif()
if(_PV_VULKAN)
  target_compile_definitions(present_core PRIVATE PV_HAVE_VULKAN=1)
endif()
set(GGML_VULKAN ${_PV_VULKAN} CACHE BOOL "" FORCE)
set(LLAMA_VULKAN ${_PV_VULKAN} CACHE BOOL "" FORCE)

# Real backends are enabled once sources exist (scripts/setup-windows.ps1 fetches them).
# Without them the DLL still builds with mock STT/LLM so the GUI + packaging work end-to-end.
# ORDER MATTERS: llama.cpp first. Both trees define a `ggml` target guarded by
# `if (NOT TARGET ggml)` — whoever is added first owns the single shared ggml,
# and llama master requires its own newer ggml (whisper's older copy lacks
# APIs llama calls, e.g. ggml_prec_set_acc). Whisper compiles fine on the
# newer ggml (stable tensor/graph API surface).
if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/thirdparty/llama.cpp/CMakeLists.txt")
    message(STATUS "present_core: llama.cpp backend ON")
    target_compile_definitions(present_core PRIVATE PV_HAVE_LLAMA=1)
    # BEFORE the subtree: opts llama's own CMake reads at configure time.
    # BOTH Vulkan switches must be forced here — llama's bundled ggml reads
    # GGML_VULKAN (not LLAMA_VULKAN) when deciding to descend into
    # ggml-vulkan, whose non-optional find_package(Vulkan) fatally aborts
    # configure on SDK-less machines. Forcing GGML_VULKAN only in the
    # whisper branch below comes too late (release CI failure 2026-10).
    set(LLAMA_VULKAN ${_PV_VULKAN} CACHE BOOL "" FORCE)
    set(GGML_VULKAN ${_PV_VULKAN} CACHE BOOL "" FORCE)
    # MTMD builds standalone (no common/tools) via its own gate.
    set(LLAMA_BUILD_MTMD ON CACHE BOOL "" FORCE)
    add_subdirectory(thirdparty/llama.cpp EXCLUDE_FROM_ALL)
    # NOTE: no `common` target — llama only builds common/ when top-level
    # (LLAMA_BUILD_COMMON); our integration uses llama.h APIs only.
    target_link_libraries(present_core PRIVATE llama)
    # Multimodal captions (Section 5b): mtmd builds standalone (no common,
    # no tools) when LLAMA_BUILD_MTMD is on. Absent tree -> mock VLM.
    if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/thirdparty/llama.cpp/tools/mtmd/CMakeLists.txt")
        message(STATUS "present_core: mtmd backend ON (vision captions)")
        target_compile_definitions(present_core PRIVATE PV_HAVE_MTMD=1)
        target_include_directories(present_core PRIVATE thirdparty/llama.cpp/tools/mtmd)
        target_link_libraries(present_core PRIVATE mtmd)
    else()
        message(STATUS "present_core: mtmd missing -> mock VLM captions")
    endif()
else()
    message(STATUS "present_core: llama.cpp missing -> mock LLM (run scripts/setup-windows.ps1)")
endif()

if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/thirdparty/whisper.cpp/CMakeLists.txt")
    message(STATUS "present_core: whisper.cpp backend ON")
    target_compile_definitions(present_core PRIVATE PV_HAVE_WHISPER=1)
    add_subdirectory(thirdparty/whisper.cpp EXCLUDE_FROM_ALL)
    add_subdirectory(thirdparty/whisper.cpp EXCLUDE_FROM_ALL)
    target_link_libraries(present_core PRIVATE whisper)
else()
    message(STATUS "present_core: whisper.cpp missing -> mock STT (run scripts/setup-windows.ps1)")
endif()

if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/thirdparty/sqlite/sqlite3.c")
    target_sources(present_core PRIVATE thirdparty/sqlite/sqlite3.c)
    target_include_directories(present_core PRIVATE thirdparty/sqlite)
    target_compile_definitions(present_core PRIVATE PV_HAVE_SQLITE=1)
else()
    message(STATUS "present_core: sqlite amalgamation missing -> file-mtime fallback only")
endif()

target_link_libraries(present_core PRIVATE dxgi)
set_target_properties(present_core PROPERTIES OUTPUT_NAME "present_core"
    CXX_STANDARD 20 CXX_STANDARD_REQUIRED ON)
