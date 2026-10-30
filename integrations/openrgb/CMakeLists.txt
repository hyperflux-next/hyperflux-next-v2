# SPDX-License-Identifier: GPL-2.0-only
#
# The HyperFlux Next V2 plugin for OpenRGB. A plugin only loads in the
# OpenRGB release whose headers it was compiled with, so OPENRGB_QT picks
# both the Qt major version and the pinned OpenRGB tree:
#
#   6  OpenRGB 1.1 with Qt 6
#   5  OpenRGB 1.0 with Qt 5
#
# OPENRGB_SOURCE is that tree as fetch.sh leaves it. `make openrgb-plugin`
# does all of this.

cmake_minimum_required(VERSION 3.20)
project(hyperflux-next-openrgb VERSION 1.0.1 LANGUAGES CXX)

set(OPENRGB_QT 6 CACHE STRING "Qt major version of the OpenRGB to build for (6 or 5)")
set(OPENRGB_SOURCE "" CACHE PATH "OpenRGB tree prepared by fetch.sh for that version")
set(HFX_PROJECT_URL "" CACHE STRING "Project page the plugin links to")
set(HFX_COMMIT "" CACHE STRING "Source revision shown in OpenRGB's plugin list")
option(HFX_WERROR "Treat compiler warnings in the plugin's own code as errors" OFF)
option(BUILD_TESTING "Build the tests" ON)

if(NOT OPENRGB_QT MATCHES "^[56]$")
    message(FATAL_ERROR "OPENRGB_QT must be 5 or 6")
endif()
if(HFX_PROJECT_URL STREQUAL "")
    message(FATAL_ERROR "set HFX_PROJECT_URL (the Makefile passes PROJECT_URL)")
endif()

# The tree must be the one pinned for this Qt version, patched by fetch.sh.
file(STRINGS "${CMAKE_CURRENT_SOURCE_DIR}/PIN" pin REGEX "^OPENRGB_QT${OPENRGB_QT}_COMMIT=")
string(REGEX REPLACE "^[^=]*=" "" pinned_commit "${pin}")
set(stamp_file "${OPENRGB_SOURCE}/.hyperflux-next-pin")
if(NOT EXISTS "${stamp_file}")
    message(FATAL_ERROR "OPENRGB_SOURCE is not a tree from fetch.sh; run integrations/openrgb/fetch.sh DIR ${OPENRGB_QT}")
endif()
file(READ "${stamp_file}" stamp)
if(NOT stamp MATCHES "^${pinned_commit} ")
    message(FATAL_ERROR "${OPENRGB_SOURCE} is not OpenRGB ${pinned_commit}; fetch it again")
endif()
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${stamp_file}" "${CMAKE_CURRENT_SOURCE_DIR}/PIN")

find_package(Qt${OPENRGB_QT} REQUIRED COMPONENTS Core Gui Widgets)
find_package(Threads REQUIRED)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
# Nothing but the Qt plugin entry points leaves the library. The copies of
# OpenRGB code inside never meet the host's.
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)
# Log lines carry source paths. Keep build directories out of the binary.
add_compile_options("-ffile-prefix-map=${OPENRGB_SOURCE}=openrgb" "-ffile-prefix-map=${PROJECT_SOURCE_DIR}=.")

set(HFX_NLOHMANN_JSON_DIR "${OPENRGB_SOURCE}/dependencies/json" CACHE PATH "" FORCE)
add_subdirectory(../../sdk/cpp sdk EXCLUDE_FROM_ALL)

set(openrgb_includes
    "${OPENRGB_SOURCE}"
    "${OPENRGB_SOURCE}/RGBController"
    "${OPENRGB_SOURCE}/KeyboardLayoutManager"
    "${OPENRGB_SOURCE}/Controllers/RazerController"
    "${OPENRGB_SOURCE}/Controllers/RazerController/RazerController"
    "${OPENRGB_SOURCE}/interop"
    "${OPENRGB_SOURCE}/dependencies/json")

# OpenRGB's Razer support, compiled into the plugin. src/ comes first so
# RazerController finds the plugin's hidapi.h.
add_library(openrgb-razer OBJECT
    "${OPENRGB_SOURCE}/Controllers/RazerController/RazerController/RazerController.cpp"
    "${OPENRGB_SOURCE}/Controllers/RazerController/RazerController/RGBController_Razer.cpp"
    "${OPENRGB_SOURCE}/Controllers/RazerController/RazerDevices.cpp"
    "${OPENRGB_SOURCE}/Controllers/RazerController/RazerDeviceGuard.cpp"
    "${OPENRGB_SOURCE}/KeyboardLayoutManager/KeyboardLayoutManager.cpp"
    "${OPENRGB_SOURCE}/RGBController/RGBController.cpp"
    "${OPENRGB_SOURCE}/RGBController/RGBControllerKeyNames.cpp"
    "${OPENRGB_SOURCE}/StringUtils.cpp"
    "${OPENRGB_SOURCE}/interop/DeviceGuard.cpp"
    "${OPENRGB_SOURCE}/interop/DeviceGuardLock.cpp"
    "${OPENRGB_SOURCE}/interop/DeviceGuardManager.cpp")
target_include_directories(openrgb-razer BEFORE PRIVATE src)
target_include_directories(openrgb-razer SYSTEM PRIVATE ${openrgb_includes})
# Upstream code is compiled as it is; its warnings are upstream's.
target_compile_options(openrgb-razer PRIVATE -w)

set(HFX_WARNINGS -Wall -Wextra -Wpedantic)
if(HFX_WERROR)
    list(APPEND HFX_WARNINGS -Werror)
endif()

# The plugin's own code, shared by the plugin and the tests.
add_library(hyperflux-openrgb OBJECT
    src/apca.cpp
    src/discovery.cpp
    src/log_manager.cpp
    src/mirror.cpp
    src/page_model.cpp
    src/page_widget.cpp
    src/route_session.cpp)
target_include_directories(hyperflux-openrgb PUBLIC src)
target_include_directories(hyperflux-openrgb SYSTEM PUBLIC ${openrgb_includes})
target_compile_options(hyperflux-openrgb PRIVATE ${HFX_WARNINGS})
target_compile_definitions(hyperflux-openrgb PUBLIC
    HFX_PROJECT_URL="${HFX_PROJECT_URL}"
    HFX_VERSION="${PROJECT_VERSION}"
    HFX_COMMIT="${HFX_COMMIT}"
    HFX_OPENRGB_QT=${OPENRGB_QT})
target_link_libraries(hyperflux-openrgb PUBLIC hyperflux-sdk Qt${OPENRGB_QT}::Widgets Threads::Threads)

configure_file(plugin.json.in hyperflux-next-openrgb.json @ONLY)

add_library(hyperflux-next-openrgb MODULE src/plugin.cpp)
set_target_properties(hyperflux-next-openrgb PROPERTIES AUTOMOC ON PREFIX "")
target_include_directories(hyperflux-next-openrgb PRIVATE "${CMAKE_CURRENT_BINARY_DIR}")
target_compile_options(hyperflux-next-openrgb PRIVATE ${HFX_WARNINGS})
target_link_libraries(hyperflux-next-openrgb PRIVATE hyperflux-openrgb openrgb-razer)
# -z defs: every symbol must be resolved here, since OpenRGB exports none.
# -z nodelete: OpenRGB unloads a plugin's library but keeps the plugin's
# widget alive in a hidden tab and destroys it at exit. That code has to
# stay mapped.
target_link_options(hyperflux-next-openrgb PRIVATE -Wl,-z,defs -Wl,-z,nodelete -Wl,--as-needed
    "-Wl,--version-script=${CMAKE_CURRENT_SOURCE_DIR}/src/plugin.map")
set_property(TARGET hyperflux-next-openrgb APPEND PROPERTY LINK_DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/src/plugin.map")

if(BUILD_TESTING)
    enable_testing()
    add_executable(test-apca tests/test_apca.cpp src/apca.cpp)
    target_include_directories(test-apca PRIVATE src)
    target_compile_options(test-apca PRIVATE ${HFX_WARNINGS})
    add_test(NAME apca COMMAND test-apca)

    add_executable(test-page-model tests/test_page_model.cpp src/page_model.cpp)
    target_include_directories(test-page-model PRIVATE src)
    target_compile_options(test-page-model PRIVATE ${HFX_WARNINGS})
    target_link_libraries(test-page-model PRIVATE hyperflux-sdk)
    add_test(NAME page-model COMMAND test-page-model)

    add_executable(test-route-session tests/test_route_session.cpp)
    target_compile_options(test-route-session PRIVATE ${HFX_WARNINGS})
    target_link_libraries(test-route-session PRIVATE hyperflux-openrgb openrgb-razer)
    add_test(NAME route-session COMMAND test-route-session)

    add_executable(test-page-widget tests/test_page_widget.cpp)
    target_compile_options(test-page-widget PRIVATE ${HFX_WARNINGS})
    target_link_libraries(test-page-widget PRIVATE hyperflux-openrgb openrgb-razer)
    add_test(NAME page-widget COMMAND test-page-widget)
    set_tests_properties(page-widget PROPERTIES ENVIRONMENT "QT_QPA_PLATFORM=offscreen")

    find_package(Python3 COMPONENTS Interpreter)
    if(Python3_FOUND)
        add_test(NAME device-table
                 COMMAND Python3::Interpreter -I "${CMAKE_CURRENT_SOURCE_DIR}/tests/check-device-table.py"
                         "${CMAKE_CURRENT_SOURCE_DIR}/src/device_table.hpp" "${OPENRGB_SOURCE}"
                         "${CMAKE_CURRENT_SOURCE_DIR}/../../devices.toml" "${OPENRGB_QT}")
    endif()
endif()
