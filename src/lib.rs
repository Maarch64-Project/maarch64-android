#![allow(non_snake_case)]
#![allow(unused_variables)]

use std::ffi::{c_char, c_void};

#[repr(C)]
pub struct NativeBridgeCallbacks {
    pub version: u32,
    pub initialize: extern "C" fn(runtime_cbs: *const c_void, private_dir: *const c_char, instruction_set: *const c_char) -> bool,
    pub loadLibrary: extern "C" fn(libpath: *const c_char, flag: i32) -> *mut c_void,
    pub getTrampoline: extern "C" fn(handle: *mut c_void, name: *const c_char, shorty: *const c_char, len: u32) -> *mut c_void,
    pub isSupported: extern "C" fn(libpath: *const c_char) -> bool,
    pub getAppEnv: extern "C" fn(instruction_set: *const c_char) -> *const c_void,
    pub isCompatibleWith: extern "C" fn(bridge_version: u32) -> bool,
    pub getSignalHandler: extern "C" fn(signal: i32) -> *mut c_void,
    pub unloadLibrary: extern "C" fn(handle: *mut c_void) -> i32,
    pub getError: extern "C" fn() -> *const c_char,
    pub isPathSupported: extern "C" fn(library_path: *const c_char) -> bool,
    pub initAnonymousNamespace: extern "C" fn(public_ns_sonames: *const c_char, anon_ns_library_path: *const c_char) -> bool,
    pub createNamespace: extern "C" fn(name: *const c_char, ld_library_path: *const c_char, default_library_path: *const c_char, type_: u64, permitted_when_isolated_path: *const c_char, parent_ns: *mut c_void) -> *mut c_void,
    pub linkNamespaces: extern "C" fn(from: *mut c_void, to: *mut c_void, shared_libs_sonames: *const c_char) -> bool,
    pub loadLibraryExt: extern "C" fn(libpath: *const c_char, flag: i32, ns: *mut c_void) -> *mut c_void,
}

extern "C" fn nb_initialize(runtime_cbs: *const c_void, private_dir: *const c_char, instruction_set: *const c_char) -> bool {
    println!("[Maarch64 NativeBridge] initialize(instruction_set={:?})", instruction_set);
    true
}

extern "C" fn nb_loadLibrary(libpath: *const c_char, flag: i32) -> *mut c_void {
    println!("[Maarch64 NativeBridge] loadLibrary(flag={})", flag);
    std::ptr::null_mut()
}

extern "C" fn nb_getTrampoline(handle: *mut c_void, name: *const c_char, shorty: *const c_char, len: u32) -> *mut c_void {
    println!("[Maarch64 NativeBridge] getTrampoline()");
    std::ptr::null_mut()
}

extern "C" fn nb_isSupported(libpath: *const c_char) -> bool {
    true
}

extern "C" fn nb_getAppEnv(instruction_set: *const c_char) -> *const c_void {
    std::ptr::null()
}

extern "C" fn nb_isCompatibleWith(bridge_version: u32) -> bool {
    true
}

extern "C" fn nb_getSignalHandler(signal: i32) -> *mut c_void {
    std::ptr::null_mut()
}

extern "C" fn nb_unloadLibrary(handle: *mut c_void) -> i32 {
    0
}

extern "C" fn nb_getError() -> *const c_char {
    std::ptr::null()
}

extern "C" fn nb_isPathSupported(library_path: *const c_char) -> bool {
    true
}

extern "C" fn nb_initAnonymousNamespace(public_ns_sonames: *const c_char, anon_ns_library_path: *const c_char) -> bool {
    true
}

extern "C" fn nb_createNamespace(name: *const c_char, ld_library_path: *const c_char, default_library_path: *const c_char, type_: u64, permitted_when_isolated_path: *const c_char, parent_ns: *mut c_void) -> *mut c_void {
    std::ptr::null_mut()
}

extern "C" fn nb_linkNamespaces(from: *mut c_void, to: *mut c_void, shared_libs_sonames: *const c_char) -> bool {
    true
}

extern "C" fn nb_loadLibraryExt(libpath: *const c_char, flag: i32, ns: *mut c_void) -> *mut c_void {
    println!("[Maarch64 NativeBridge] loadLibraryExt()");
    std::ptr::null_mut()
}

#[no_mangle]
pub static NativeBridgeItf: NativeBridgeCallbacks = NativeBridgeCallbacks {
    version: 3,
    initialize: nb_initialize,
    loadLibrary: nb_loadLibrary,
    getTrampoline: nb_getTrampoline,
    isSupported: nb_isSupported,
    getAppEnv: nb_getAppEnv,
    isCompatibleWith: nb_isCompatibleWith,
    getSignalHandler: nb_getSignalHandler,
    unloadLibrary: nb_unloadLibrary,
    getError: nb_getError,
    isPathSupported: nb_isPathSupported,
    initAnonymousNamespace: nb_initAnonymousNamespace,
    createNamespace: nb_createNamespace,
    linkNamespaces: nb_linkNamespaces,
    loadLibraryExt: nb_loadLibraryExt,
};
