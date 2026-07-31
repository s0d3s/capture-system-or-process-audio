get_filename_component(_CSPA_CMAKE_DIR "${CMAKE_CURRENT_LIST_FILE}" PATH)
get_filename_component(CSPA_PACKAGE_PREFIX "${_CSPA_CMAKE_DIR}/../../.." ABSOLUTE)

set(CSPA_INCLUDE_DIR "${CSPA_PACKAGE_PREFIX}/include")

if(NOT EXISTS "${CSPA_INCLUDE_DIR}/cspa_experimental.h")
    message(FATAL_ERROR "Cspa package is missing cspa_experimental.h at ${CSPA_INCLUDE_DIR}")
endif()
if(NOT EXISTS "${CSPA_INCLUDE_DIR}/cspa.hpp")
    message(FATAL_ERROR "Cspa package is missing cspa.hpp at ${CSPA_INCLUDE_DIR}")
endif()

if(WIN32)
    set(CSPA_LIBRARY "${CSPA_PACKAGE_PREFIX}/lib/cspa_ffi.dll.lib")
    set(CSPA_RUNTIME_DLL "${CSPA_PACKAGE_PREFIX}/bin/cspa_ffi.dll")
    if(NOT EXISTS "${CSPA_LIBRARY}")
        message(FATAL_ERROR "Cspa package is missing import library at ${CSPA_LIBRARY}")
    endif()
    if(NOT EXISTS "${CSPA_RUNTIME_DLL}")
        message(FATAL_ERROR "Cspa package is missing runtime library at ${CSPA_RUNTIME_DLL}")
    endif()
    if(NOT TARGET Cspa::cspa_ffi)
        add_library(Cspa::cspa_ffi SHARED IMPORTED)
        set_target_properties(Cspa::cspa_ffi PROPERTIES
            IMPORTED_IMPLIB "${CSPA_LIBRARY}"
            IMPORTED_LOCATION "${CSPA_RUNTIME_DLL}"
            INTERFACE_INCLUDE_DIRECTORIES "${CSPA_INCLUDE_DIR}"
        )
    endif()
elseif(APPLE)
    set(CSPA_LIBRARY "${CSPA_PACKAGE_PREFIX}/lib/libcspa_ffi.dylib")
    if(NOT EXISTS "${CSPA_LIBRARY}")
        message(FATAL_ERROR "Cspa package is missing shared library at ${CSPA_LIBRARY}")
    endif()
    if(NOT TARGET Cspa::cspa_ffi)
        add_library(Cspa::cspa_ffi SHARED IMPORTED)
        set_target_properties(Cspa::cspa_ffi PROPERTIES
            IMPORTED_LOCATION "${CSPA_LIBRARY}"
            INTERFACE_INCLUDE_DIRECTORIES "${CSPA_INCLUDE_DIR}"
        )
    endif()
else()
    set(CSPA_LIBRARY "${CSPA_PACKAGE_PREFIX}/lib/libcspa_ffi.so")
    if(NOT EXISTS "${CSPA_LIBRARY}")
        message(FATAL_ERROR "Cspa package is missing shared library at ${CSPA_LIBRARY}")
    endif()
    if(NOT TARGET Cspa::cspa_ffi)
        add_library(Cspa::cspa_ffi SHARED IMPORTED)
        set_target_properties(Cspa::cspa_ffi PROPERTIES
            IMPORTED_LOCATION "${CSPA_LIBRARY}"
            INTERFACE_INCLUDE_DIRECTORIES "${CSPA_INCLUDE_DIR}"
        )
    endif()
endif()

set(Cspa_FOUND TRUE)
