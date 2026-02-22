# CMake toolchain file for aarch64-apple-darwin (macOS arm64).
#
# Used by boring-sys2 when building BoringSSL for this target, loaded via
# CMAKE_TOOLCHAIN_FILE_aarch64_apple_darwin set in .cargo/config.toml.
#
# When the cmake HOST is Linux (inside rb-sys-dock) it is a true cross-
# compilation setup: we point cmake at the osxcross compilers and, critically,
# set CMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY so that cmake validates the
# compiler by building a static library instead of a full executable. Without
# this, the link step invokes the host /usr/bin/ld (GNU ld) which does not
# understand the macOS -dynamic flag and aborts.
#
# When the cmake HOST is Darwin (native macOS build) every osxcross-specific
# setting is skipped, so this file is effectively a no-op and normal cmake
# compiler detection proceeds unchanged.

set(CMAKE_SYSTEM_NAME Darwin)
set(CMAKE_SYSTEM_PROCESSOR arm64)
set(CMAKE_OSX_ARCHITECTURES arm64)
set(CMAKE_OSX_DEPLOYMENT_TARGET "10.13")

if(CMAKE_HOST_SYSTEM_NAME STREQUAL "Linux")
    # osxcross compiler wrappers (available inside the rb-sys-dock container)
    set(CMAKE_C_COMPILER   aarch64-apple-darwin-cc)
    set(CMAKE_CXX_COMPILER aarch64-apple-darwin-c++)
    set(CMAKE_ASM_COMPILER aarch64-apple-darwin-cc)
    set(CMAKE_LINKER       aarch64-apple-darwin-ld)

    # Skip the executable link test — the host GNU ld doesn't understand -dynamic.
    set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

    set(CMAKE_OSX_SYSROOT /opt/osxcross/target/SDK/MacOSX11.1.sdk)

    # Don't search host paths for libraries / programs / includes.
    set(CMAKE_FIND_ROOT_PATH /opt/osxcross/target)
    set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
    set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
    set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
endif()
