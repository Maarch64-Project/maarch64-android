/// JNI stub function IDs — each maps to a thunk address in guest memory.
/// The thunk address encodes the function identity so we can demux in the thunk handler.
pub const JNI_STUB_COUNT: usize = 64;

/// Offsets within the JNIEnv function table (each pointer = 8 bytes on AArch64).
pub mod jnienv_slot {
    pub const RESERVED0: usize = 0;
    pub const RESERVED1: usize = 1;
    pub const RESERVED2: usize = 2;
    pub const RESERVED3: usize = 3;
    pub const GET_VERSION: usize = 4;
    pub const DEFINE_CLASS: usize = 5;
    pub const FIND_CLASS: usize = 6;
    pub const FROM_REFLECTED_METHOD: usize = 7;
    pub const FROM_REFLECTED_FIELD: usize = 8;
    pub const TO_REFLECTED_METHOD: usize = 9;
    pub const GET_SUPERCLASS: usize = 10;
    pub const IS_ASSIGNABLE_FROM: usize = 11;
    pub const TO_REFLECTED_FIELD: usize = 12;
    pub const THROW: usize = 13;
    pub const THROW_NEW: usize = 14;
    pub const EXCEPTION_OCCURRED: usize = 15;
    pub const EXCEPTION_DESCRIBE: usize = 16;
    pub const EXCEPTION_CLEAR: usize = 17;
    pub const FATAL_ERROR: usize = 18;
    pub const PUSH_LOCAL_FRAME: usize = 19;
    pub const POP_LOCAL_FRAME: usize = 20;
    pub const NEW_GLOBAL_REF: usize = 21;
    pub const DELETE_GLOBAL_REF: usize = 22;
    pub const DELETE_LOCAL_REF: usize = 23;
    pub const IS_SAME_OBJECT: usize = 24;
    pub const NEW_LOCAL_REF: usize = 25;
    pub const ENSURE_LOCAL_CAPACITY: usize = 26;
    pub const ALLOC_OBJECT: usize = 27;
    pub const NEW_OBJECT: usize = 28;
    pub const GET_OBJECT_CLASS: usize = 29;
    pub const IS_INSTANCE_OF: usize = 30;
    pub const GET_METHOD_ID: usize = 31;  // 33 in JNI spec but 0-indexed
    pub const CALL_OBJECT_METHOD: usize = 34;
    pub const CALL_VOID_METHOD: usize = 40;
    pub const CALL_BOOLEAN_METHOD: usize = 37;
    pub const CALL_INT_METHOD: usize = 38;
    pub const CALL_LONG_METHOD: usize = 39;
    pub const GET_STATIC_METHOD_ID: usize = 113;
    pub const CALL_STATIC_VOID_METHOD: usize = 116;
    pub const CALL_STATIC_OBJECT_METHOD: usize = 114;
    pub const NEW_STRING_UTF: usize = 167;
    pub const GET_STRING_UTF_CHARS: usize = 169;
    pub const RELEASE_STRING_UTF_CHARS: usize = 170;
    pub const GET_ARRAY_LENGTH: usize = 171;
    pub const GET_FIELD_ID: usize = 94;
    pub const GET_STATIC_FIELD_ID: usize = 144;
    pub const GET_OBJECT_FIELD: usize = 95;
    pub const GET_INT_FIELD: usize = 98;
    pub const REGISTER_NATIVES: usize = 215;
    pub const UNREGISTER_NATIVES: usize = 216;
    pub const MONITOR_ENTER: usize = 217;
    pub const MONITOR_EXIT: usize = 218;
    pub const GET_JAVA_VM: usize = 219;
}

/// JavaVM function table slot indices.
pub mod javavm_slot {
    pub const RESERVED0: usize = 0;
    pub const RESERVED1: usize = 1;
    pub const RESERVED2: usize = 2;
    pub const DESTROY_JAVA_VM: usize = 3;
    pub const ATTACH_CURRENT_THREAD: usize = 4;
    pub const DETACH_CURRENT_THREAD: usize = 5;
    pub const GET_ENV: usize = 6;
    pub const ATTACH_CURRENT_THREAD_AS_DAEMON: usize = 7;
}

/// Thunk base addresses for JNI dispatch demuxing.
/// Each JNI function gets its own fixed guest address.
pub const JNIENV_THUNK_BASE: u64 = 0x7f10_0000;
pub const JAVAVM_THUNK_BASE: u64 = 0x7f11_0000;
pub const JNI_THUNK_STRIDE: u64 = 8;

/// Returns the guest address of a specific JNIEnv function stub.
pub fn jnienv_thunk_addr(slot: usize) -> u64 {
    JNIENV_THUNK_BASE + (slot as u64) * JNI_THUNK_STRIDE
}

/// Returns the guest address of a specific JavaVM function stub.
pub fn javavm_thunk_addr(slot: usize) -> u64 {
    JAVAVM_THUNK_BASE + (slot as u64) * JNI_THUNK_STRIDE
}

/// Identifies a JNI stub by its guest PC address.
#[derive(Debug, Clone, PartialEq)]
pub enum JniStubId {
    // JNIEnv functions
    FindClass,
    GetMethodId,
    GetStaticMethodId,
    CallVoidMethod,
    CallObjectMethod,
    CallIntMethod,
    CallBooleanMethod,
    CallLongMethod,
    CallStaticVoidMethod,
    CallStaticObjectMethod,
    NewStringUtf,
    GetStringUtfChars,
    ReleaseStringUtfChars,
    GetVersion,
    ExceptionOccurred,
    ExceptionDescribe,
    ExceptionClear,
    NewGlobalRef,
    DeleteGlobalRef,
    DeleteLocalRef,
    GetObjectClass,
    IsInstanceOf,
    GetFieldId,
    GetStaticFieldId,
    GetObjectField,
    GetIntField,
    RegisterNatives,
    GetJavaVm,
    GetArrayLength,
    MonitorEnter,
    MonitorExit,
    // JavaVM functions
    GetEnv,
    AttachCurrentThread,
    DetachCurrentThread,
    // Fallback
    Unknown(u64),
}

impl JniStubId {
    pub fn from_pc(pc: u64) -> Self {
        if pc >= JNIENV_THUNK_BASE && pc < JAVAVM_THUNK_BASE {
            let slot = ((pc - JNIENV_THUNK_BASE) / JNI_THUNK_STRIDE) as usize;
            match slot {
                s if s == jnienv_slot::FIND_CLASS => Self::FindClass,
                s if s == jnienv_slot::GET_METHOD_ID => Self::GetMethodId,
                s if s == jnienv_slot::GET_STATIC_METHOD_ID => Self::GetStaticMethodId,
                s if s == jnienv_slot::CALL_VOID_METHOD => Self::CallVoidMethod,
                s if s == jnienv_slot::CALL_OBJECT_METHOD => Self::CallObjectMethod,
                s if s == jnienv_slot::CALL_INT_METHOD => Self::CallIntMethod,
                s if s == jnienv_slot::CALL_BOOLEAN_METHOD => Self::CallBooleanMethod,
                s if s == jnienv_slot::CALL_LONG_METHOD => Self::CallLongMethod,
                s if s == jnienv_slot::CALL_STATIC_VOID_METHOD => Self::CallStaticVoidMethod,
                s if s == jnienv_slot::CALL_STATIC_OBJECT_METHOD => Self::CallStaticObjectMethod,
                s if s == jnienv_slot::NEW_STRING_UTF => Self::NewStringUtf,
                s if s == jnienv_slot::GET_STRING_UTF_CHARS => Self::GetStringUtfChars,
                s if s == jnienv_slot::RELEASE_STRING_UTF_CHARS => Self::ReleaseStringUtfChars,
                s if s == jnienv_slot::GET_VERSION => Self::GetVersion,
                s if s == jnienv_slot::EXCEPTION_OCCURRED => Self::ExceptionOccurred,
                s if s == jnienv_slot::EXCEPTION_DESCRIBE => Self::ExceptionDescribe,
                s if s == jnienv_slot::EXCEPTION_CLEAR => Self::ExceptionClear,
                s if s == jnienv_slot::NEW_GLOBAL_REF => Self::NewGlobalRef,
                s if s == jnienv_slot::DELETE_GLOBAL_REF => Self::DeleteGlobalRef,
                s if s == jnienv_slot::DELETE_LOCAL_REF => Self::DeleteLocalRef,
                s if s == jnienv_slot::GET_OBJECT_CLASS => Self::GetObjectClass,
                s if s == jnienv_slot::IS_INSTANCE_OF => Self::IsInstanceOf,
                s if s == jnienv_slot::GET_FIELD_ID => Self::GetFieldId,
                s if s == jnienv_slot::GET_STATIC_FIELD_ID => Self::GetStaticFieldId,
                s if s == jnienv_slot::GET_OBJECT_FIELD => Self::GetObjectField,
                s if s == jnienv_slot::GET_INT_FIELD => Self::GetIntField,
                s if s == jnienv_slot::REGISTER_NATIVES => Self::RegisterNatives,
                s if s == jnienv_slot::GET_JAVA_VM => Self::GetJavaVm,
                s if s == jnienv_slot::GET_ARRAY_LENGTH => Self::GetArrayLength,
                s if s == jnienv_slot::MONITOR_ENTER => Self::MonitorEnter,
                s if s == jnienv_slot::MONITOR_EXIT => Self::MonitorExit,
                _ => Self::Unknown(pc),
            }
        } else if pc >= JAVAVM_THUNK_BASE && pc < JAVAVM_THUNK_BASE + 0x1000 {
            let slot = ((pc - JAVAVM_THUNK_BASE) / JNI_THUNK_STRIDE) as usize;
            match slot {
                s if s == javavm_slot::GET_ENV => Self::GetEnv,
                s if s == javavm_slot::ATTACH_CURRENT_THREAD => Self::AttachCurrentThread,
                s if s == javavm_slot::ATTACH_CURRENT_THREAD_AS_DAEMON => Self::AttachCurrentThread,
                s if s == javavm_slot::DETACH_CURRENT_THREAD => Self::DetachCurrentThread,
                _ => Self::Unknown(pc),
            }
        } else {
            Self::Unknown(pc)
        }
    }
}
