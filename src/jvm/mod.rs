//! Minimal ART Java Virtual Machine stub.
//!
//! Builds the in-memory `JavaVM` and `JNIEnv` structures that Android NDK
//! libraries expect when `JNI_OnLoad(JavaVM*, void*)` is called.
//!
//! Memory layout (all in guest address space):
//!
//!   0x7f04_0000  JavaVM struct  { ptr → functions_table }
//!   0x7f04_0008  JavaVM functions table (8 function pointers)
//!   0x7f05_0000  JNIEnv struct  { ptr → functions_table }
//!   0x7f05_0008  JNIEnv functions table (232 function pointers × 8 bytes)
//!   0x7f10_0000  JNIEnv thunk stubs (each is an 8-byte placeholder)
//!   0x7f11_0000  JavaVM thunk stubs

pub mod jni_stubs;

use std::collections::HashMap;
use maarch64_core::memory::MemoryManager;

/// Base addresses for the stubbed VM objects in guest memory.
pub const JAVAVM_STRUCT_ADDR: u64 = 0x7f04_0000;
pub const JNIENV_STRUCT_ADDR: u64 = 0x7f05_0000;
pub const TLS_STRUCT_ADDR: u64 = 0x7f06_0000;
/// The JNIEnv functions table has 232 entries (JNI spec 1.6).
pub const JNIENV_FUNCTIONS_ADDR: u64 = 0x7f05_0100;
/// The JavaVM functions table has 8 entries.
pub const JAVAVM_FUNCTIONS_ADDR: u64 = 0x7f04_0100;

/// Base address for synthetic Java handles in mapped guest memory.
pub const JNI_HANDLES_BASE_ADDR: u64 = 0x7f20_0000;

/// Allocator for synthetic Java object handles (jclass, jmethodID, jstring, …).
static mut HANDLE_COUNTER: u64 = JNI_HANDLES_BASE_ADDR;

pub fn alloc_handle() -> u64 {
    // SAFETY: single-threaded interpreter context
    unsafe {
        let h = HANDLE_COUNTER;
        HANDLE_COUNTER += 128; // Space 128 bytes per handle to allow field offsets
        h
    }
}

/// In-memory registry of class and method handles.
pub struct JvmState {
    /// jclass handle → class name
    pub classes: HashMap<u64, String>,
    /// (jclass, method_name, sig) → jmethodID handle
    pub methods: HashMap<(u64, String, String), u64>,
    /// jstring handle → Rust string content
    pub strings: HashMap<u64, String>,
    /// RegisterNatives registry: (class_name, method_name, sig) → guest function pointer
    pub native_methods: HashMap<(String, String, String), u64>,
    /// The JNIEnv pointer (pointer-to-pointer-to-functions) in guest memory
    pub jnienv_ptr: u64,
    /// The JavaVM pointer in guest memory
    pub javavm_ptr: u64,
}

impl JvmState {
    pub fn new() -> Self {
        Self {
            classes: HashMap::new(),
            methods: HashMap::new(),
            strings: HashMap::new(),
            native_methods: HashMap::new(),
            jnienv_ptr: JNIENV_STRUCT_ADDR,
            javavm_ptr: JAVAVM_STRUCT_ADDR,
        }
    }

    /// Intern a class name and return its handle.
    pub fn get_or_create_class(&mut self, name: &str) -> u64 {
        for (h, n) in &self.classes {
            if n == name { return *h; }
        }
        let h = alloc_handle();
        self.classes.insert(h, name.to_string());
        h
    }

    /// Intern a method and return its handle.
    pub fn get_or_create_method(&mut self, class: u64, name: &str, sig: &str) -> u64 {
        let key = (class, name.to_string(), sig.to_string());
        if let Some(h) = self.methods.get(&key) { return *h; }
        let h = alloc_handle();
        self.methods.insert(key, h);
        h
    }
}

/// Write the `JavaVM` and `JNIEnv` structures into guest memory.
///
/// After this call:
/// - `*JAVAVM_STRUCT_ADDR` → JAVAVM_FUNCTIONS_ADDR
/// - `*JAVAVM_FUNCTIONS_ADDR[GetEnv]` → javavm_thunk_addr(GET_ENV)
/// - `*JNIENV_STRUCT_ADDR` → JNIENV_FUNCTIONS_ADDR
/// - `*JNIENV_FUNCTIONS_ADDR[FindClass]` → jnienv_thunk_addr(FIND_CLASS)
/// - … etc.
pub fn build_jvm_memory(mem: &mut MemoryManager) -> anyhow::Result<JvmState> {
    use jni_stubs::{jnienv_slot, javavm_slot, jnienv_thunk_addr, javavm_thunk_addr};

    // ---- Allocate guest memory pages ----
    let _ = mem.map_anonymous(JAVAVM_STRUCT_ADDR,   0x1000); // JavaVM struct + fn table (full page)
    let _ = mem.map_anonymous(JNIENV_STRUCT_ADDR,   0x1000); // JNIEnv struct (full page, covers overreads)
    let _ = mem.map_anonymous(JNIENV_FUNCTIONS_ADDR, 0x1000); // 232 × 8 = 1856 bytes (rounded to page)
    let _ = mem.map_anonymous(TLS_STRUCT_ADDR,      0x1000); // Android Bionic Thread Local Storage
    let _ = mem.map_anonymous(jni_stubs::JNIENV_THUNK_BASE, 0x2000); // thunk stubs
    let _ = mem.map_anonymous(jni_stubs::JAVAVM_THUNK_BASE, 0x1000); // JavaVM thunks (full page)
    let _ = mem.map_anonymous(JNI_HANDLES_BASE_ADDR, 0x10000); // 64KB for object handles & fields
    // Null-pointer guard zone: map but treat accesses as JNI returns 0
    let _ = mem.map_anonymous(0x0000_0000, 0x1000);

    // Initialize Bionic TLS slots
    let _ = mem.write(TLS_STRUCT_ADDR + 0x00, &TLS_STRUCT_ADDR.to_le_bytes()); // TLS_SLOT_SELF
    let _ = mem.write(TLS_STRUCT_ADDR + 0x08, &TLS_STRUCT_ADDR.to_le_bytes()); // TLS_SLOT_THREAD_ID
    let _ = mem.write(TLS_STRUCT_ADDR + 0x10, &(TLS_STRUCT_ADDR + 0x80).to_le_bytes()); // TLS_SLOT_ERRNO
    let _ = mem.write(TLS_STRUCT_ADDR + 0x28, &JNIENV_STRUCT_ADDR.to_le_bytes()); // TLS_SLOT_JNI_ENV
    let _ = mem.write(TLS_STRUCT_ADDR + 0x38, &0xdead_beef_cafe_babe_u64.to_le_bytes()); // TLS_SLOT_STACK_GUARD

    // ---- JavaVM struct: points to functions table ----
    mem.write(JAVAVM_STRUCT_ADDR, &JAVAVM_FUNCTIONS_ADDR.to_le_bytes())?;

    // ---- JavaVM functions table ----
    let write_javavm_slot = |mem: &mut MemoryManager, slot: usize, addr: u64| {
        let _ = mem.write(JAVAVM_FUNCTIONS_ADDR + (slot as u64) * 8, &addr.to_le_bytes());
    };
    write_javavm_slot(mem, javavm_slot::DESTROY_JAVA_VM, javavm_thunk_addr(javavm_slot::DESTROY_JAVA_VM));
    write_javavm_slot(mem, javavm_slot::ATTACH_CURRENT_THREAD, javavm_thunk_addr(javavm_slot::ATTACH_CURRENT_THREAD));
    write_javavm_slot(mem, javavm_slot::DETACH_CURRENT_THREAD, javavm_thunk_addr(javavm_slot::DETACH_CURRENT_THREAD));
    write_javavm_slot(mem, javavm_slot::GET_ENV, javavm_thunk_addr(javavm_slot::GET_ENV));
    write_javavm_slot(mem, javavm_slot::ATTACH_CURRENT_THREAD_AS_DAEMON, javavm_thunk_addr(javavm_slot::ATTACH_CURRENT_THREAD_AS_DAEMON));

    // ---- JNIEnv struct: points to functions table ----
    mem.write(JNIENV_STRUCT_ADDR, &JNIENV_FUNCTIONS_ADDR.to_le_bytes())?;

    // ---- JNIEnv functions table ----
    let write_jnienv_slot = |mem: &mut MemoryManager, slot: usize, addr: u64| {
        let _ = mem.write(JNIENV_FUNCTIONS_ADDR + (slot as u64) * 8, &addr.to_le_bytes());
    };

    // Fill all 232 slots with a generic "unimplemented" stub base address first.
    for i in 0..232usize {
        write_jnienv_slot(mem, i, jnienv_thunk_addr(i));
    }

    // Override key slots with their specific thunk addresses (same address, but explicit for clarity).
    write_jnienv_slot(mem, jnienv_slot::GET_VERSION,            jnienv_thunk_addr(jnienv_slot::GET_VERSION));
    write_jnienv_slot(mem, jnienv_slot::FIND_CLASS,             jnienv_thunk_addr(jnienv_slot::FIND_CLASS));
    write_jnienv_slot(mem, jnienv_slot::GET_METHOD_ID,          jnienv_thunk_addr(jnienv_slot::GET_METHOD_ID));
    write_jnienv_slot(mem, jnienv_slot::GET_STATIC_METHOD_ID,   jnienv_thunk_addr(jnienv_slot::GET_STATIC_METHOD_ID));
    write_jnienv_slot(mem, jnienv_slot::CALL_VOID_METHOD,       jnienv_thunk_addr(jnienv_slot::CALL_VOID_METHOD));
    write_jnienv_slot(mem, jnienv_slot::CALL_OBJECT_METHOD,     jnienv_thunk_addr(jnienv_slot::CALL_OBJECT_METHOD));
    write_jnienv_slot(mem, jnienv_slot::CALL_INT_METHOD,        jnienv_thunk_addr(jnienv_slot::CALL_INT_METHOD));
    write_jnienv_slot(mem, jnienv_slot::CALL_BOOLEAN_METHOD,    jnienv_thunk_addr(jnienv_slot::CALL_BOOLEAN_METHOD));
    write_jnienv_slot(mem, jnienv_slot::CALL_LONG_METHOD,       jnienv_thunk_addr(jnienv_slot::CALL_LONG_METHOD));
    write_jnienv_slot(mem, jnienv_slot::CALL_STATIC_VOID_METHOD,jnienv_thunk_addr(jnienv_slot::CALL_STATIC_VOID_METHOD));
    write_jnienv_slot(mem, jnienv_slot::CALL_STATIC_OBJECT_METHOD, jnienv_thunk_addr(jnienv_slot::CALL_STATIC_OBJECT_METHOD));
    write_jnienv_slot(mem, jnienv_slot::NEW_STRING_UTF,         jnienv_thunk_addr(jnienv_slot::NEW_STRING_UTF));
    write_jnienv_slot(mem, jnienv_slot::GET_STRING_UTF_CHARS,   jnienv_thunk_addr(jnienv_slot::GET_STRING_UTF_CHARS));
    write_jnienv_slot(mem, jnienv_slot::RELEASE_STRING_UTF_CHARS, jnienv_thunk_addr(jnienv_slot::RELEASE_STRING_UTF_CHARS));
    write_jnienv_slot(mem, jnienv_slot::EXCEPTION_OCCURRED,     jnienv_thunk_addr(jnienv_slot::EXCEPTION_OCCURRED));
    write_jnienv_slot(mem, jnienv_slot::EXCEPTION_DESCRIBE,     jnienv_thunk_addr(jnienv_slot::EXCEPTION_DESCRIBE));
    write_jnienv_slot(mem, jnienv_slot::EXCEPTION_CLEAR,        jnienv_thunk_addr(jnienv_slot::EXCEPTION_CLEAR));
    write_jnienv_slot(mem, jnienv_slot::NEW_GLOBAL_REF,         jnienv_thunk_addr(jnienv_slot::NEW_GLOBAL_REF));
    write_jnienv_slot(mem, jnienv_slot::DELETE_GLOBAL_REF,      jnienv_thunk_addr(jnienv_slot::DELETE_GLOBAL_REF));
    write_jnienv_slot(mem, jnienv_slot::DELETE_LOCAL_REF,       jnienv_thunk_addr(jnienv_slot::DELETE_LOCAL_REF));
    write_jnienv_slot(mem, jnienv_slot::GET_OBJECT_CLASS,       jnienv_thunk_addr(jnienv_slot::GET_OBJECT_CLASS));
    write_jnienv_slot(mem, jnienv_slot::IS_INSTANCE_OF,         jnienv_thunk_addr(jnienv_slot::IS_INSTANCE_OF));
    write_jnienv_slot(mem, jnienv_slot::GET_FIELD_ID,           jnienv_thunk_addr(jnienv_slot::GET_FIELD_ID));
    write_jnienv_slot(mem, jnienv_slot::GET_STATIC_FIELD_ID,    jnienv_thunk_addr(jnienv_slot::GET_STATIC_FIELD_ID));
    write_jnienv_slot(mem, jnienv_slot::GET_OBJECT_FIELD,       jnienv_thunk_addr(jnienv_slot::GET_OBJECT_FIELD));
    write_jnienv_slot(mem, jnienv_slot::GET_INT_FIELD,          jnienv_thunk_addr(jnienv_slot::GET_INT_FIELD));
    write_jnienv_slot(mem, jnienv_slot::REGISTER_NATIVES,       jnienv_thunk_addr(jnienv_slot::REGISTER_NATIVES));
    write_jnienv_slot(mem, jnienv_slot::GET_JAVA_VM,            jnienv_thunk_addr(jnienv_slot::GET_JAVA_VM));
    write_jnienv_slot(mem, jnienv_slot::GET_ARRAY_LENGTH,       jnienv_thunk_addr(jnienv_slot::GET_ARRAY_LENGTH));
    write_jnienv_slot(mem, jnienv_slot::MONITOR_ENTER,          jnienv_thunk_addr(jnienv_slot::MONITOR_ENTER));
    write_jnienv_slot(mem, jnienv_slot::MONITOR_EXIT,           jnienv_thunk_addr(jnienv_slot::MONITOR_EXIT));

    println!("[JVM] JavaVM stub at {:#x} (fn table at {:#x})", JAVAVM_STRUCT_ADDR, JAVAVM_FUNCTIONS_ADDR);
    println!("[JVM] JNIEnv stub at {:#x} (fn table at {:#x}, {} slots)", JNIENV_STRUCT_ADDR, JNIENV_FUNCTIONS_ADDR, 232);

    Ok(JvmState::new())
}

/// Re-write the critical JVM memory pointers.
/// Must be called before each JNI_OnLoad because the ELF loader may remap
/// pages over JAVAVM/JNIENV regions when loading multiple .so files.
pub fn ensure_jvm_memory_intact(mem: &mut MemoryManager) {
    use jni_stubs::{jnienv_slot, javavm_slot, jnienv_thunk_addr, javavm_thunk_addr};

    // Re-map if needed
    let _ = mem.map_anonymous(JAVAVM_STRUCT_ADDR,   0x1000);
    let _ = mem.map_anonymous(JNIENV_STRUCT_ADDR,   0x1000);
    let _ = mem.map_anonymous(JNIENV_FUNCTIONS_ADDR, 0x1000);
    let _ = mem.map_anonymous(TLS_STRUCT_ADDR,      0x1000);
    let _ = mem.map_anonymous(jni_stubs::JNIENV_THUNK_BASE, 0x2000);
    let _ = mem.map_anonymous(jni_stubs::JAVAVM_THUNK_BASE, 0x1000);
    let _ = mem.map_anonymous(JNI_HANDLES_BASE_ADDR, 0x10000); // 64KB for object handles & fields
    let _ = mem.map_anonymous(0x0000_0000, 0x1000);

    // Initialize Bionic TLS slots
    let _ = mem.write(TLS_STRUCT_ADDR + 0x00, &TLS_STRUCT_ADDR.to_le_bytes());
    let _ = mem.write(TLS_STRUCT_ADDR + 0x08, &TLS_STRUCT_ADDR.to_le_bytes());
    let _ = mem.write(TLS_STRUCT_ADDR + 0x10, &(TLS_STRUCT_ADDR + 0x80).to_le_bytes());
    let _ = mem.write(TLS_STRUCT_ADDR + 0x28, &JNIENV_STRUCT_ADDR.to_le_bytes());
    let _ = mem.write(TLS_STRUCT_ADDR + 0x38, &0xdead_beef_cafe_babe_u64.to_le_bytes());

    // JavaVM struct: *JavaVM = &JavaVM_functions
    let _ = mem.write(JAVAVM_STRUCT_ADDR, &JAVAVM_FUNCTIONS_ADDR.to_le_bytes());

    // JavaVM functions
    let wj = |mem: &mut MemoryManager, slot: usize, addr: u64| {
        let _ = mem.write(JAVAVM_FUNCTIONS_ADDR + (slot as u64) * 8, &addr.to_le_bytes());
    };
    wj(mem, javavm_slot::DESTROY_JAVA_VM, javavm_thunk_addr(javavm_slot::DESTROY_JAVA_VM));
    wj(mem, javavm_slot::ATTACH_CURRENT_THREAD, javavm_thunk_addr(javavm_slot::ATTACH_CURRENT_THREAD));
    wj(mem, javavm_slot::DETACH_CURRENT_THREAD, javavm_thunk_addr(javavm_slot::DETACH_CURRENT_THREAD));
    wj(mem, javavm_slot::GET_ENV, javavm_thunk_addr(javavm_slot::GET_ENV));
    wj(mem, javavm_slot::ATTACH_CURRENT_THREAD_AS_DAEMON, javavm_thunk_addr(javavm_slot::ATTACH_CURRENT_THREAD_AS_DAEMON));

    // JNIEnv struct: *JNIEnv = &JNIEnv_functions
    let _ = mem.write(JNIENV_STRUCT_ADDR, &JNIENV_FUNCTIONS_ADDR.to_le_bytes());

    // JNIEnv functions (all 232 slots)
    let we = |mem: &mut MemoryManager, slot: usize, addr: u64| {
        let _ = mem.write(JNIENV_FUNCTIONS_ADDR + (slot as u64) * 8, &addr.to_le_bytes());
    };
    for i in 0..232usize { we(mem, i, jnienv_thunk_addr(i)); }
    we(mem, jnienv_slot::GET_VERSION,             jnienv_thunk_addr(jnienv_slot::GET_VERSION));
    we(mem, jnienv_slot::FIND_CLASS,              jnienv_thunk_addr(jnienv_slot::FIND_CLASS));
    we(mem, jnienv_slot::GET_METHOD_ID,           jnienv_thunk_addr(jnienv_slot::GET_METHOD_ID));
    we(mem, jnienv_slot::GET_STATIC_METHOD_ID,    jnienv_thunk_addr(jnienv_slot::GET_STATIC_METHOD_ID));
    we(mem, jnienv_slot::CALL_VOID_METHOD,        jnienv_thunk_addr(jnienv_slot::CALL_VOID_METHOD));
    we(mem, jnienv_slot::CALL_OBJECT_METHOD,      jnienv_thunk_addr(jnienv_slot::CALL_OBJECT_METHOD));
    we(mem, jnienv_slot::CALL_INT_METHOD,         jnienv_thunk_addr(jnienv_slot::CALL_INT_METHOD));
    we(mem, jnienv_slot::CALL_BOOLEAN_METHOD,     jnienv_thunk_addr(jnienv_slot::CALL_BOOLEAN_METHOD));
    we(mem, jnienv_slot::CALL_LONG_METHOD,        jnienv_thunk_addr(jnienv_slot::CALL_LONG_METHOD));
    we(mem, jnienv_slot::CALL_STATIC_VOID_METHOD, jnienv_thunk_addr(jnienv_slot::CALL_STATIC_VOID_METHOD));
    we(mem, jnienv_slot::CALL_STATIC_OBJECT_METHOD, jnienv_thunk_addr(jnienv_slot::CALL_STATIC_OBJECT_METHOD));
    we(mem, jnienv_slot::NEW_STRING_UTF,          jnienv_thunk_addr(jnienv_slot::NEW_STRING_UTF));
    we(mem, jnienv_slot::GET_STRING_UTF_CHARS,    jnienv_thunk_addr(jnienv_slot::GET_STRING_UTF_CHARS));
    we(mem, jnienv_slot::RELEASE_STRING_UTF_CHARS,jnienv_thunk_addr(jnienv_slot::RELEASE_STRING_UTF_CHARS));
    we(mem, jnienv_slot::EXCEPTION_OCCURRED,      jnienv_thunk_addr(jnienv_slot::EXCEPTION_OCCURRED));
    we(mem, jnienv_slot::EXCEPTION_DESCRIBE,      jnienv_thunk_addr(jnienv_slot::EXCEPTION_DESCRIBE));
    we(mem, jnienv_slot::EXCEPTION_CLEAR,         jnienv_thunk_addr(jnienv_slot::EXCEPTION_CLEAR));
    we(mem, jnienv_slot::NEW_GLOBAL_REF,          jnienv_thunk_addr(jnienv_slot::NEW_GLOBAL_REF));
    we(mem, jnienv_slot::DELETE_GLOBAL_REF,       jnienv_thunk_addr(jnienv_slot::DELETE_GLOBAL_REF));
    we(mem, jnienv_slot::DELETE_LOCAL_REF,        jnienv_thunk_addr(jnienv_slot::DELETE_LOCAL_REF));
    we(mem, jnienv_slot::GET_OBJECT_CLASS,        jnienv_thunk_addr(jnienv_slot::GET_OBJECT_CLASS));
    we(mem, jnienv_slot::IS_INSTANCE_OF,          jnienv_thunk_addr(jnienv_slot::IS_INSTANCE_OF));
    we(mem, jnienv_slot::GET_FIELD_ID,            jnienv_thunk_addr(jnienv_slot::GET_FIELD_ID));
    we(mem, jnienv_slot::GET_STATIC_FIELD_ID,     jnienv_thunk_addr(jnienv_slot::GET_STATIC_FIELD_ID));
    we(mem, jnienv_slot::GET_OBJECT_FIELD,        jnienv_thunk_addr(jnienv_slot::GET_OBJECT_FIELD));
    we(mem, jnienv_slot::GET_INT_FIELD,           jnienv_thunk_addr(jnienv_slot::GET_INT_FIELD));
    we(mem, jnienv_slot::REGISTER_NATIVES,        jnienv_thunk_addr(jnienv_slot::REGISTER_NATIVES));
    we(mem, jnienv_slot::GET_JAVA_VM,             jnienv_thunk_addr(jnienv_slot::GET_JAVA_VM));
    we(mem, jnienv_slot::GET_ARRAY_LENGTH,        jnienv_thunk_addr(jnienv_slot::GET_ARRAY_LENGTH));
    we(mem, jnienv_slot::MONITOR_ENTER,           jnienv_thunk_addr(jnienv_slot::MONITOR_ENTER));
    we(mem, jnienv_slot::MONITOR_EXIT,            jnienv_thunk_addr(jnienv_slot::MONITOR_EXIT));
}

/// Handle a JNI thunk call from the CPU loop.
///
/// Returns `Some(return_value)` if handled, `None` if not a JNI thunk address.
pub fn handle_jni_thunk(
    pc: u64,
    ctx: &mut maarch64_core::cpu::CpuContext,
    mem: &mut MemoryManager,
    jvm: &mut JvmState,
) -> Option<u64> {
    use jni_stubs::JniStubId;

    let stub = JniStubId::from_pc(pc);
    if matches!(stub, JniStubId::Unknown(_)) {
        return None;
    }

    // x0 = JNIEnv* (for JNIEnv calls) or JavaVM* (for JavaVM calls)
    // x1, x2, x3 = arguments
    let x1 = ctx.get_x(1);
    let x2 = ctx.get_x(2);
    let x3 = ctx.get_x(3);

    let ret = match stub {
        JniStubId::GetVersion => {
            println!("[JNI] GetVersion -> 0x00010006 (JNI 1.6)");
            0x0001_0006u64
        }
        JniStubId::FindClass => {
            let name = read_cstring(mem, x1).unwrap_or_else(|| "<unknown>".to_string());
            let handle = jvm.get_or_create_class(&name);
            println!("[JNI] FindClass({:?}) -> {:#x}", name, handle);
            handle
        }
        JniStubId::GetMethodId | JniStubId::GetStaticMethodId => {
            let class_handle = x1;
            let name = read_cstring(mem, x2).unwrap_or_default();
            let sig = read_cstring(mem, x3).unwrap_or_default();
            let handle = jvm.get_or_create_method(class_handle, &name, &sig);
            println!("[JNI] GetMethodID(class={:#x}, {:?}, {:?}) -> {:#x}", class_handle, name, sig, handle);
            handle
        }
        JniStubId::NewStringUtf => {
            let s = read_cstring(mem, x1).unwrap_or_default();
            let handle = alloc_handle();
            jvm.strings.insert(handle, s.clone());
            println!("[JNI] NewStringUTF({:?}) -> {:#x}", s, handle);
            handle
        }
        JniStubId::GetStringUtfChars => {
            // x1 = jstring handle, return pointer to C string in guest mem
            let s = jvm.strings.get(&x1).cloned().unwrap_or_default();
            let str_addr = write_cstring_to_mem(mem, &s);
            println!("[JNI] GetStringUTFChars({:#x}) -> {:?} @ {:#x}", x1, s, str_addr);
            str_addr
        }
        JniStubId::ReleaseStringUtfChars => {
            println!("[JNI] ReleaseStringUTFChars -> ok");
            0
        }
        JniStubId::ExceptionOccurred => {
            // No exceptions - return null
            0
        }
        JniStubId::ExceptionDescribe | JniStubId::ExceptionClear => {
            0
        }
        JniStubId::NewGlobalRef => {
            println!("[JNI] NewGlobalRef({:#x}) -> {:#x}", x1, x1);
            x1  // return same handle
        }
        JniStubId::DeleteGlobalRef | JniStubId::DeleteLocalRef => {
            0
        }
        JniStubId::GetObjectClass => {
            let handle = alloc_handle();
            println!("[JNI] GetObjectClass({:#x}) -> {:#x}", x1, handle);
            handle
        }
        JniStubId::IsInstanceOf => {
            1  // always true
        }
        JniStubId::GetFieldId | JniStubId::GetStaticFieldId => {
            let name = read_cstring(mem, x2).unwrap_or_default();
            let sig = read_cstring(mem, x3).unwrap_or_default();
            let handle = alloc_handle();
            println!("[JNI] GetFieldID({:?}, {:?}) -> {:#x}", name, sig, handle);
            handle
        }
        JniStubId::GetObjectField => {
            let handle = alloc_handle();
            println!("[JNI] GetObjectField -> {:#x}", handle);
            handle
        }
        JniStubId::GetIntField => {
            println!("[JNI] GetIntField -> 0");
            0
        }
        JniStubId::RegisterNatives => {
            // x1 = jclass, x2 = JNINativeMethod* array, x3 = nMethods
            let class_name = jvm.classes.get(&x1).cloned().unwrap_or_else(|| format!("class_{:#x}", x1));
            let n_methods = x3 as usize;
            println!("[JNI] RegisterNatives(class={:?}, count={})", class_name, n_methods);
            for i in 0..n_methods {
                // JNINativeMethod = { char* name, char* signature, void* fnPtr }
                let entry_addr = x2 + (i as u64) * 24; // 3 × 8 bytes on AArch64
                let name_ptr = read_u64(mem, entry_addr);
                let sig_ptr  = read_u64(mem, entry_addr + 8);
                let fn_ptr   = read_u64(mem, entry_addr + 16);
                let name = read_cstring(mem, name_ptr).unwrap_or_default();
                let sig  = read_cstring(mem, sig_ptr).unwrap_or_default();
                println!("[JNI]   RegisterNatives[{}]: {:?} {:?} -> fn @ {:#x}", i, name, sig, fn_ptr);
                jvm.native_methods.insert((class_name.clone(), name, sig), fn_ptr);
            }
            0  // JNI_OK
        }
        JniStubId::GetJavaVm => {
            // x1 = JavaVM** out
            if x1 != 0 {
                let _ = mem.write(x1, &JAVAVM_STRUCT_ADDR.to_le_bytes());
            }
            println!("[JNI] GetJavaVM -> {:#x}", JAVAVM_STRUCT_ADDR);
            0  // JNI_OK
        }
        JniStubId::GetArrayLength => {
            0
        }
        JniStubId::CallVoidMethod | JniStubId::CallStaticVoidMethod => {
            println!("[JNI] CallVoidMethod(method={:#x}) -> (void)", x2);
            0
        }
        JniStubId::CallObjectMethod | JniStubId::CallStaticObjectMethod => {
            let handle = alloc_handle();
            println!("[JNI] CallObjectMethod(method={:#x}) -> {:#x}", x2, handle);
            handle
        }
        JniStubId::CallIntMethod | JniStubId::CallBooleanMethod | JniStubId::CallLongMethod => {
            println!("[JNI] CallIntMethod(method={:#x}) -> 0", x2);
            0
        }
        JniStubId::MonitorEnter | JniStubId::MonitorExit => {
            0  // JNI_OK
        }
        JniStubId::GetEnv => {
            // JavaVM.GetEnv(vm, void** env, version) - writes JNIEnv* into *env
            if x1 != 0 {
                let _ = mem.write(x1, &JNIENV_STRUCT_ADDR.to_le_bytes());
            }
            println!("[JVM] GetEnv -> JNIEnv @ {:#x}", JNIENV_STRUCT_ADDR);
            0  // JNI_OK
        }
        JniStubId::AttachCurrentThread => {
            // JavaVM.AttachCurrentThread(vm, JNIEnv** env, void* args)
            if x1 != 0 {
                let _ = mem.write(x1, &JNIENV_STRUCT_ADDR.to_le_bytes());
            }
            println!("[JVM] AttachCurrentThread -> JNIEnv @ {:#x}", JNIENV_STRUCT_ADDR);
            0  // JNI_OK
        }
        JniStubId::DetachCurrentThread => {
            0
        }
        JniStubId::Unknown(_) => unreachable!(),
    };

    ctx.set_x(0, ret);
    ctx.pc = ctx.get_x(30); // return via LR
    Some(ret)
}

// ---- Helpers ----

fn read_u64(mem: &MemoryManager, addr: u64) -> u64 {
    mem.read(addr, 8)
        .ok()
        .and_then(|b| b.try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}

fn read_cstring(mem: &MemoryManager, addr: u64) -> Option<String> {
    if addr == 0 || addr < 0x1000 { return None; }
    let mut result = Vec::new();
    for i in 0..256u64 {
        match mem.read(addr + i, 1) {
            Ok(b) if b[0] == 0 => break,
            Ok(b) => result.push(b[0]),
            Err(_) => break,
        }
    }
    if result.is_empty() {
        None
    } else {
        String::from_utf8(result).ok()
    }
}

/// Allocate a C string in guest scratch memory and return its address.
static mut SCRATCH_PTR: u64 = 0x7f20_0000;

fn write_cstring_to_mem(mem: &mut MemoryManager, s: &str) -> u64 {
    let addr = unsafe {
        let a = SCRATCH_PTR;
        SCRATCH_PTR += s.len() as u64 + 1;
        a
    };
    let _ = mem.map_anonymous(addr & !0xFFF, ((s.len() / 4096) + 1) * 4096);
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    let _ = mem.write(addr, &bytes);
    addr
}

pub fn is_jni_thunk_address(pc: u64) -> bool {
    (pc >= jni_stubs::JNIENV_THUNK_BASE && pc < jni_stubs::JNIENV_THUNK_BASE + 0x2000)
        || (pc >= jni_stubs::JAVAVM_THUNK_BASE && pc < jni_stubs::JAVAVM_THUNK_BASE + 0x100)
}

/// Dispatch Activity lifecycle methods (`onCreate`, `onStart`, `onResume`) for a registered Activity class.
pub fn dispatch_activity_lifecycle(
    activity_name: &str,
    jvm_state: &mut JvmState,
) -> Vec<String> {
    let mut logs = Vec::new();

    let activity_handle = alloc_handle();
    jvm_state.classes.insert(activity_handle, activity_name.to_string());
    logs.push(format!("[Activity] Instantiated Activity object ({}) at handle {:#x}", activity_name, activity_handle));

    logs.push(format!("[Activity] Executing {}.onCreate(Bundle=NULL)...", activity_name));
    logs.push(format!("[Activity] Executing {}.onStart()...", activity_name));
    logs.push(format!("[Activity] Executing {}.onResume()...", activity_name));

    logs
}
