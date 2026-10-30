# SPDX-License-Identifier: GPL-2.0-only

cmake_minimum_required(VERSION 3.20)
project(hyperflux-sdk VERSION 1.0.2 LANGUAGES CXX)

# The plugin builds with the nlohmann/json copy inside the pinned OpenRGB
# tree, and so does sdk-test. Without a directory, an installed nlohmann_json
# package is used.
set(HFX_NLOHMANN_JSON_DIR "" CACHE PATH "Directory containing nlohmann/json.hpp")
option(HFX_WERROR "Treat compiler warnings as errors" OFF)

add_library(hyperflux-sdk STATIC src/client.cpp src/wire.cpp)
target_include_directories(hyperflux-sdk PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/include")
target_compile_features(hyperflux-sdk PUBLIC cxx_std_20)
set_target_properties(hyperflux-sdk PROPERTIES POSITION_INDEPENDENT_CODE ON)
if(HFX_NLOHMANN_JSON_DIR)
    target_include_directories(hyperflux-sdk SYSTEM PUBLIC "${HFX_NLOHMANN_JSON_DIR}")
else()
    find_package(nlohmann_json 3.11 REQUIRED CONFIG)
    target_link_libraries(hyperflux-sdk PUBLIC nlohmann_json::nlohmann_json)
endif()

set(HFX_WARNINGS -Wall -Wextra -Wpedantic)
if(HFX_WERROR)
    list(APPEND HFX_WARNINGS -Werror)
endif()
target_compile_options(hyperflux-sdk PRIVATE ${HFX_WARNINGS})

if(PROJECT_IS_TOP_LEVEL)
    enable_testing()
    find_package(Threads REQUIRED)
    add_executable(hyperflux-sdk-test tests/test_client.cpp)
    target_link_libraries(hyperflux-sdk-test PRIVATE hyperflux-sdk Threads::Threads)
    target_compile_options(hyperflux-sdk-test PRIVATE ${HFX_WARNINGS})
    target_compile_definitions(hyperflux-sdk-test PRIVATE
        HFX_FIXTURES="${CMAKE_CURRENT_SOURCE_DIR}/../../crates/hfx-proto/tests/fixtures")
    add_test(NAME hyperflux-sdk-test COMMAND hyperflux-sdk-test)
endif()
