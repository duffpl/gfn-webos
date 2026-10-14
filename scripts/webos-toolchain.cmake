# CMake toolchain for webOS cross builds (used by -sys crates through the cmake crate). Without it
# CMake on macOS archives with Apple's ar, whose BSD-format archives GNU ld silently skips.
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR arm)
set(_gfn_sdk "$ENV{WEBOS_SDK}")
set(CMAKE_SYSROOT "${_gfn_sdk}/arm-webos-linux-gnueabi/sysroot")
set(CMAKE_C_COMPILER "${CMAKE_CURRENT_LIST_DIR}/arm-webos-gcc")
set(CMAKE_CXX_COMPILER "${CMAKE_CURRENT_LIST_DIR}/arm-webos-g++")
set(CMAKE_AR "${_gfn_sdk}/bin/arm-webos-linux-gnueabi-ar" CACHE FILEPATH "")
set(CMAKE_RANLIB "${_gfn_sdk}/bin/arm-webos-linux-gnueabi-ranlib" CACHE FILEPATH "")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
