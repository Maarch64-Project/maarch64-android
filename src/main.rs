mod jvm;
mod dex;

use clap::Parser;
use maarch64_core::{
    cpu::CpuContext,
    interp::Interpreter,
    jit::JitEngine,
    loader::{AutoLoader, TargetOs},
    memory::MemoryManager,
};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;
use zip::ZipArchive;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Maarch64 Linux-Native Android Runtime (APK & NDK)",
    long_about = "Executes ARM64 Android APKs and native shared libraries (.so) directly on Linux x86_64"
)]
struct Args {
    /// Path to target Android .apk, .apkm, .xapk or arm64-v8a .so library
    #[arg(value_name = "APK_OR_SO")]
    target: PathBuf,

    /// Enable Cranelift JIT compilation
    #[arg(long)]
    jit: bool,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Override which .so to load (by filename basename)
    #[arg(long, value_name = "SO_NAME")]
    so: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let filter = if args.verbose {
        EnvFilter::new("info")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"))
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    println!("============================================================");
    println!("  Maarch64 Linux-Native Android Runtime (Not-An-Emulator)  ");
    println!("============================================================");

    let target_path = &args.target;
    let mut mem = MemoryManager::new();

    let ext = target_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let is_archive = matches!(ext.to_lowercase().as_str(), "apk" | "apkm" | "xapk" | "apks" | "zip");

    if is_archive {
        println!("[+] Target is Android App Bundle / Archive ({:?}): {:?}", ext, target_path);
        run_apk_bundle(&args, target_path, &mut mem)?;
    } else {
        println!("[+] Target is Android Native Binary: {:?}", target_path);
        run_native_so(&args, target_path, &mut mem)?;
    }

    Ok(())
}

/// Run a native .so directly (NDK NativeActivity path).
fn run_native_so(args: &Args, so_path: &Path, mem: &mut MemoryManager) -> anyhow::Result<()> {
    println!("[+] Loading Android Native ARM64 binary: {:?}", so_path);
    let path_str = so_path.to_string_lossy();
    let loaded = AutoLoader::load_file_with_args(so_path, &[&path_str], mem)?;

    println!(
        "[+] Loaded Android Binary (Target OS: {:?}, Entry: {:#x})",
        TargetOs::Android,
        loaded.entry_point
    );

    let mut ctx = CpuContext::new();
    ctx.pc = loaded.entry_point;
    ctx.sp = loaded.stack_pointer;
    ctx.target_os = TargetOs::Android;

    // Allocate ANativeActivity struct
    let activity_ptr = mem.map_anonymous(0x7f03_0000, 256).unwrap_or(0x7f03_0000);
    let callbacks_ptr = activity_ptr + 128;
    let _ = mem.write(activity_ptr, &callbacks_ptr.to_le_bytes());

    ctx.set_x(0, activity_ptr);
    ctx.set_x(1, 0);
    ctx.set_x(2, 0);

    let mut thunk_manager = maarch64_thunks::ThunkManager::new();
    for (addr, name) in &loaded.dynamic_thunks {
        thunk_manager.resolve_dynamic_symbol(name, *addr);
    }

    println!("[+] Registered {} dynamic symbols with Android ThunkManager", loaded.dynamic_thunks.len());
    println!("[+] Starting Android Native Execution (JIT={})...", args.jit);

    let mut jit_engine = JitEngine::new();
    let mut step_count = 0u64;
    let mut jvm_state = jvm::JvmState::new();

    run_cpu_loop(&mut ctx, mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit, &mut jvm_state)?;

    let on_start_ptr = read_u64(mem, callbacks_ptr);
    let on_resume_ptr = read_u64(mem, callbacks_ptr + 8);
    let on_window_created_ptr = read_u64(mem, callbacks_ptr + 56);

    if on_start_ptr != 0 {
        println!("[+] Invoking ANativeActivity onStart callback ({:#x})...", on_start_ptr);
        ctx.pc = on_start_ptr;
        ctx.set_x(0, activity_ptr);
        ctx.set_x(30, 0);
        let _ = run_cpu_loop(&mut ctx, mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit, &mut jvm_state);
    }

    if on_resume_ptr != 0 {
        println!("[+] Invoking ANativeActivity onResume callback ({:#x})...", on_resume_ptr);
        ctx.pc = on_resume_ptr;
        ctx.set_x(0, activity_ptr);
        ctx.set_x(30, 0);
        let _ = run_cpu_loop(&mut ctx, mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit, &mut jvm_state);
    }

    if on_window_created_ptr != 0 {
        println!("[+] Creating Native Window for ANativeActivity onNativeWindowCreated ({:#x})...", on_window_created_ptr);
        let _ = maarch64_thunks::gpu::thunk_XCreateWindow(&mut ctx, mem);
        let win_handle = ctx.get_x(0);
        ctx.pc = on_window_created_ptr;
        ctx.set_x(0, activity_ptr);
        ctx.set_x(1, win_handle);
        ctx.set_x(30, 0);
        let _ = run_cpu_loop(&mut ctx, mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit, &mut jvm_state);
    } else {
        println!("[i] Note: Target binary does not export ANativeActivity_onCreate (Native UI Window lifecycle callback).");
        println!("[i] Java/Kotlin Android apps (like LINE) manage UI rendering via Java ART VM & Android View framework, whereas Pure C/C++ NDK apps (NativeActivity / SDL / Raylib) create Native GUI Windows directly.");
    }

    println!("[+] Android Native Execution Finished Cleanly (Steps: {})", step_count);
    Ok(())
}

/// Run a Java/Kotlin APK bundle via JNI bridge.
fn run_apk_bundle(args: &Args, archive_path: &Path, mem: &mut MemoryManager) -> anyhow::Result<()> {
    let out_dir = std::env::temp_dir().join("maarch64_apk_extracted");
    fs::create_dir_all(&out_dir)?;

    // Step 1: Extract AndroidManifest.xml from base.apk
    let manifest_info = extract_manifest(archive_path);
    if let Some(ref pkg) = manifest_info.package_name {
        println!("[APK] Package: {}", pkg);
    }
    if let Some(ref app) = manifest_info.application_class {
        println!("[APK] Application class: {}", app);
    }
    if let Some(ref act) = manifest_info.main_activity {
        println!("[APK] Main Activity: {}", act);
    }

    // Step 2: Extract all ARM64 .so files from the bundle
    let so_files = extract_all_native_sos(archive_path, &out_dir)?;
    if so_files.is_empty() {
        anyhow::bail!("No ARM64 native libraries found in archive");
    }
    println!("[APK] Extracted {} ARM64 native libraries", so_files.len());

    // Step 3: Build JavaVM / JNIEnv stubs in guest memory
    let mut jvm_state = jvm::build_jvm_memory(mem)?;

    // Step 4: Find .so files that export JNI_OnLoad and invoke them
    let target_so = args.so.as_deref();
    let jni_sos = find_jni_onload_libs(&so_files, target_so);

    if jni_sos.is_empty() {
        println!("[JVM] No JNI_OnLoad libraries found. Trying first available .so...");
        if let Some(first) = so_files.first() {
            run_native_so(args, first, mem)?;
        }
        return Ok(());
    }

    println!("[JVM] Found {} JNI_OnLoad libraries. Invoking...", jni_sos.len());

    for so_path in &jni_sos {
        if let Err(e) = invoke_jni_onload(args, so_path, mem, &mut jvm_state) {
            println!("[JVM] JNI_OnLoad in {:?} failed: {:?}", so_path.file_name().unwrap_or_default(), e);
        }
    }

    println!("[JVM] JNI phase complete. Registered {} native methods.", jvm_state.native_methods.len());
    for ((class, method, sig), fn_ptr) in &jvm_state.native_methods {
        println!("[JVM]   {}::{}{} -> {:#x}", class, method, sig, fn_ptr);
    }

    println!("[+] Android JNI Execution Finished Cleanly");
    Ok(())
}

/// Invoke `JNI_OnLoad(JavaVM*, void*)` in a loaded .so.
fn invoke_jni_onload(args: &Args, so_path: &Path, mem: &mut MemoryManager, jvm_state: &mut jvm::JvmState) -> anyhow::Result<()> {
    let name = so_path.file_name().unwrap_or_default().to_string_lossy();
    println!("[JVM] Loading {} for JNI_OnLoad...", name);

    let path_str = so_path.to_string_lossy();
    let loaded = AutoLoader::load_file_with_args(so_path, &[&path_str], mem)?;

    // Find JNI_OnLoad symbol address
    let jni_onload_addr = loaded.dynamic_thunks.iter()
        .find(|(_, sym)| sym == "JNI_OnLoad")
        .map(|(addr, _)| *addr);

    // Also check ELF symbol table directly
    let jni_entry = if let Some(addr) = jni_onload_addr {
        addr
    } else {
        // Use the loader's found entry if it detected JNI_OnLoad
        find_jni_onload_in_elf(so_path)?
    };

    println!("[JVM] Calling JNI_OnLoad @ {:#x} in {}", jni_entry, name);

    let mut ctx = CpuContext::new();
    ctx.pc = jni_entry;
    ctx.sp = loaded.stack_pointer;
    ctx.target_os = TargetOs::Android;

    // JNI_OnLoad(JavaVM* vm, void* reserved)
    ctx.set_x(0, jvm::JAVAVM_STRUCT_ADDR);
    ctx.set_x(1, 0); // reserved = NULL
    ctx.set_x(30, 0); // return to NULL -> terminates

    let mut thunk_manager = maarch64_thunks::ThunkManager::new();
    for (addr, sym_name) in &loaded.dynamic_thunks {
        thunk_manager.resolve_dynamic_symbol(sym_name, *addr);
    }

    let mut jit_engine = JitEngine::new();
    let mut step_count = 0u64;

    run_cpu_loop(&mut ctx, mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit, jvm_state)?;

    let jni_version = ctx.get_x(0);
    println!("[JVM] JNI_OnLoad returned JNI version {:#x} (steps: {})", jni_version, step_count);
    Ok(())
}

/// Find `JNI_OnLoad` entry point in an ELF .so file's symbol table.
fn find_jni_onload_in_elf(so_path: &Path) -> anyhow::Result<u64> {
    use object::{Object, ObjectSymbol};
    let data = fs::read(so_path)?;
    let file = object::File::parse(&*data)
        .map_err(|e| anyhow::anyhow!("ELF parse error: {}", e))?;

    let load_bias: u64 = if file.segments().next()
        .map(|s| { use object::ObjectSegment; s.address() })
        .unwrap_or(0) == 0 { 0x400000 } else { 0 };

    for sym in file.dynamic_symbols().chain(file.symbols()) {
        if let Ok(name) = sym.name() {
            if name == "JNI_OnLoad" {
                return Ok(sym.address() + load_bias);
            }
        }
    }
    anyhow::bail!("JNI_OnLoad not found in {:?}", so_path)
}

/// Returns which .so files export `JNI_OnLoad`.
fn find_jni_onload_libs(so_files: &[PathBuf], filter: Option<&str>) -> Vec<PathBuf> {
    use object::{Object, ObjectSymbol};
    so_files.iter()
        .filter(|p| {
            if let Some(name) = filter {
                return p.file_name().map(|n| n.to_string_lossy().contains(name)).unwrap_or(false);
            }
            true
        })
        .filter(|p| {
            let data = match fs::read(p) {
                Ok(d) => d,
                Err(_) => return false,
            };
            object::File::parse(data.as_slice())
                .map(|f| {
                    f.dynamic_symbols().chain(f.symbols())
                        .any(|s| s.name().ok() == Some("JNI_OnLoad"))
                })
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Extract ALL ARM64 .so files from the bundle into `out_dir`.
fn extract_all_native_sos(archive_path: &Path, out_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let file = File::open(archive_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut result = Vec::new();

    // Direct search in primary archive
    for i in 0..archive.len() {
        let mut zip_file = archive.by_index(i)?;
        let name = zip_file.name().to_string();
        if name.starts_with("lib/arm64-v8a/") && name.ends_with(".so") {
            let fname = Path::new(&name).file_name().unwrap_or_default();
            let out_path = out_dir.join(fname);
            let mut buf = Vec::new();
            zip_file.read_to_end(&mut buf)?;
            fs::write(&out_path, &buf)?;
            result.push(out_path);
        }
    }

    // Search nested split APKs
    let mut nested = Vec::new();
    for i in 0..archive.len() {
        let f = archive.by_index(i)?;
        let n = f.name().to_string();
        if n.ends_with(".apk") || n.ends_with(".apks") { nested.push(n); }
    }

    for nested_name in nested {
        let mut nested_file = archive.by_name(&nested_name)?;
        let mut buf = Vec::new();
        nested_file.read_to_end(&mut buf)?;
        if let Ok(mut nested_archive) = ZipArchive::new(std::io::Cursor::new(buf)) {
            for i in 0..nested_archive.len() {
                let mut zip_file = nested_archive.by_index(i)?;
                let name = zip_file.name().to_string();
                if name.starts_with("lib/arm64-v8a/") && name.ends_with(".so") {
                    let fname = Path::new(&name).file_name().unwrap_or_default();
                    let out_path = out_dir.join(fname);
                    if !out_path.exists() {
                        let mut content = Vec::new();
                        zip_file.read_to_end(&mut content)?;
                        fs::write(&out_path, &content)?;
                        result.push(out_path);
                    }
                }
            }
        }
    }

    Ok(result)
}

/// Extract `AndroidManifest.xml` from `base.apk` inside a bundle, or directly from APK.
fn extract_manifest(archive_path: &Path) -> dex::ManifestInfo {
    let try_parse = |data: &[u8]| dex::ManifestInfo::parse(data);

    // Try direct APK
    if let Ok(file) = File::open(archive_path) {
        if let Ok(mut archive) = ZipArchive::new(file) {
            // Try nested base.apk first
            if let Ok(mut nested_file) = archive.by_name("base.apk") {
                let mut buf = Vec::new();
                if nested_file.read_to_end(&mut buf).is_ok() {
                    if let Ok(mut nested) = ZipArchive::new(std::io::Cursor::new(buf)) {
                        if let Ok(mut mf) = nested.by_name("AndroidManifest.xml") {
                            let mut data = Vec::new();
                            if mf.read_to_end(&mut data).is_ok() {
                                return try_parse(&data);
                            }
                        }
                    }
                }
            }
            // Direct manifest
            if let Ok(mut mf) = archive.by_name("AndroidManifest.xml") {
                let mut data = Vec::new();
                if mf.read_to_end(&mut data).is_ok() {
                    return try_parse(&data);
                }
            }
        }
    }

    dex::ManifestInfo { main_activity: None, application_class: None, package_name: None }
}

/// Extract native so using arch priority (for direct .so execution path).
fn extract_apk_native_so(archive_path: &Path) -> anyhow::Result<PathBuf> {
    let file = File::open(archive_path)?;
    let mut archive = ZipArchive::new(file)?;

    let out_dir = std::env::temp_dir().join("maarch64_apk_extracted");
    fs::create_dir_all(&out_dir)?;

    let arch_priorities = [
        "lib/arm64-v8a/",
        "lib/x86_64/",
        "lib/armeabi-v7a/",
        "lib/armeabi/",
        "lib/",
    ];

    for arch in &arch_priorities {
        for i in 0..archive.len() {
            let mut zip_file = archive.by_index(i)?;
            let name = zip_file.name().to_string();
            if name.starts_with(arch) && name.ends_with(".so") {
                let is_64bit = arch.contains("arm64") || arch.contains("x86_64");
                println!("[+] Found NDK shared library in root archive ({}): {}", arch, name);
                if !is_64bit {
                    println!("[!] Warning: Selected library is 32-bit ({}). Maarch64 target architecture is ARM64 (AArch64).", arch);
                }
                let file_basename = Path::new(&name).file_name().unwrap_or_default();
                let out_file_path = out_dir.join(file_basename);
                let mut buffer = Vec::new();
                zip_file.read_to_end(&mut buffer)?;
                fs::write(&out_file_path, &buffer)?;
                println!("[+] Extracted native library to: {:?}", out_file_path);
                return Ok(out_file_path);
            }
        }
    }

    let mut nested_apk_names = Vec::new();
    for i in 0..archive.len() {
        let zip_file = archive.by_index(i)?;
        let name = zip_file.name().to_string();
        if name.ends_with(".apk") || name.ends_with(".apks") {
            nested_apk_names.push(name);
        }
    }

    for nested_name in nested_apk_names {
        println!("[+] Scanning nested split APK inside bundle: {}", nested_name);
        let mut nested_file = archive.by_name(&nested_name)?;
        let mut nested_buf = Vec::new();
        nested_file.read_to_end(&mut nested_buf)?;

        if let Ok(mut nested_archive) = ZipArchive::new(std::io::Cursor::new(nested_buf)) {
            for arch in &arch_priorities {
                for i in 0..nested_archive.len() {
                    let mut zip_file = nested_archive.by_index(i)?;
                    let name = zip_file.name().to_string();
                    if name.starts_with(arch) && name.ends_with(".so") {
                        let is_64bit = arch.contains("arm64") || arch.contains("x86_64");
                        println!("[+] Found NDK shared library in split APK ({}, {}): {}", nested_name, arch, name);
                        if !is_64bit {
                            println!("[!] Warning: Selected library is 32-bit ({}). Maarch64 target architecture is ARM64 (AArch64).", arch);
                        }
                        let file_basename = Path::new(&name).file_name().unwrap_or_default();
                        let out_file_path = out_dir.join(file_basename);
                        let mut buffer = Vec::new();
                        zip_file.read_to_end(&mut buffer)?;
                        fs::write(&out_file_path, &buffer)?;
                        println!("[+] Extracted native library to: {:?}", out_file_path);
                        return Ok(out_file_path);
                    }
                }
            }
        }
    }

    anyhow::bail!("No native shared library (*.so) found in archive or nested split APKs")
}

fn read_u64(mem: &MemoryManager, addr: u64) -> u64 {
    if let Ok(bytes) = mem.read(addr, 8) {
        u64::from_le_bytes(bytes.try_into().unwrap_or_default())
    } else {
        0
    }
}

fn run_cpu_loop(
    ctx: &mut CpuContext,
    mem: &mut MemoryManager,
    thunk_manager: &maarch64_thunks::ThunkManager,
    jit_engine: &mut JitEngine,
    step_count: &mut u64,
    use_jit: bool,
    jvm_state: &mut jvm::JvmState,
) -> anyhow::Result<()> {
    loop {
        *step_count += 1;
        if *step_count >= 50_000_000 {
            eprintln!("[!] Reached max execution step limit.");
            break;
        }

        // Check JNI thunk addresses first
        if jvm::is_jni_thunk_address(ctx.pc) {
            let pc = ctx.pc;
            jvm::handle_jni_thunk(pc, ctx, mem, jvm_state);
            continue;
        }

        if let Some(thunk) = thunk_manager.get_thunk_by_address(ctx.pc) {
            let entry_pc = ctx.pc;
            let _ = thunk(ctx, mem);
            if ctx.exited {
                break;
            }
            if ctx.pc == entry_pc {
                ctx.pc = ctx.get_x(30);
            }
            continue;
        }

        if ctx.pc == 0 {
            tracing::info!("[+] Function returned to NULL (PC=0), finishing execution.");
            break;
        }

        let res = if use_jit {
            jit_engine.execute(ctx, mem)
        } else {
            Interpreter::step(ctx, mem)
        };

        match res {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                eprintln!("[!] Android Runtime Execution Error: {:?}", e);
                break;
            }
        }
    }
    Ok(())
}
