cmake_minimum_required(VERSION 3.24)
project(present_voice LANGUAGES C CXX)

# Inherited default for thirdparty trees (whisper pins its own 17, llama
# targets must NOT see C++20: u8 literals become char8_t and break MSVC).
# present_core itself opts back up to 20 via target properties.
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

option(PV_ENABLE_VULKAN "Build whisper/llama backends with Vulkan (NVIDIA+AMD)" ON)
option(PV_BUNDLE_FFMPEG "Expect FFmpeg shared DLLs beside the exe (all-formats decode)" ON)

add_subdirectory(core)
