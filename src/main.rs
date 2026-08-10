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
    /// Path to target Android .apk or arm64-v8a .so library
    #[arg(value_name = "APK_OR_SO")]
    target: PathBuf,

    /// Enable Cranelift JIT compilation
    #[arg(long)]
    jit: bool,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let filter = if args.verbose {
        EnvFilter::new("info")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
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

    let binary_path = if target_path.extension().and_then(|e| e.to_str()) == Some("apk") {
        println!("[+] Target is APK archive: {:?}", target_path);
        extract_apk_native_so(target_path)?
    } else {
        target_path.clone()
    };

    println!("[+] Loading Android Native ARM64 binary: {:?}", binary_path);
    let path_str = binary_path.to_string_lossy();
    let loaded = AutoLoader::load_file_with_args(&binary_path, &[&path_str], &mut mem)?;

    println!(
        "[+] Loaded Android Binary (Target OS: {:?}, Entry: {:#x})",
        TargetOs::Android,
        loaded.entry_point
    );

    let mut ctx = CpuContext::new();
    ctx.pc = loaded.entry_point;
    ctx.sp = loaded.stack_pointer;
    ctx.target_os = TargetOs::Android;

    // Allocate ANativeActivity memory structure
    let activity_ptr = mem.map_anonymous(0x7f03_0000, 256).unwrap_or(0x7f03_0000);
    let callbacks_ptr = activity_ptr + 128;
    let _ = mem.write(activity_ptr, &callbacks_ptr.to_le_bytes());

    // Pass ANativeActivity struct as first argument (x0)
    ctx.set_x(0, activity_ptr);
    ctx.set_x(1, 0); // savedState
    ctx.set_x(2, 0); // savedStateSize

    let mut thunk_manager = maarch64_thunks::ThunkManager::new();
    for (addr, name) in &loaded.dynamic_thunks {
        thunk_manager.resolve_dynamic_symbol(name, *addr);
    }

    println!(
        "[+] Registered {} dynamic symbols with Android ThunkManager",
        loaded.dynamic_thunks.len()
    );

    println!("[+] Starting Android Native Execution (JIT={})...", args.jit);
    let mut jit_engine = JitEngine::new();
    let mut step_count = 0u64;

    // Run ANativeActivity_onCreate or main entry point
    run_cpu_loop(&mut ctx, &mut mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit)?;

    // Read callbacks table
    let on_start_ptr = read_u64(&mem, callbacks_ptr);
    let on_resume_ptr = read_u64(&mem, callbacks_ptr + 8);
    let on_window_created_ptr = read_u64(&mem, callbacks_ptr + 56);

    if on_start_ptr != 0 {
        println!("[+] Invoking ANativeActivity onStart callback ({:#x})...", on_start_ptr);
        ctx.pc = on_start_ptr;
        ctx.set_x(0, activity_ptr);
        ctx.set_x(30, 0);
        let _ = run_cpu_loop(&mut ctx, &mut mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit);
    }

    if on_resume_ptr != 0 {
        println!("[+] Invoking ANativeActivity onResume callback ({:#x})...", on_resume_ptr);
        ctx.pc = on_resume_ptr;
        ctx.set_x(0, activity_ptr);
        ctx.set_x(30, 0);
        let _ = run_cpu_loop(&mut ctx, &mut mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit);
    }

    if on_window_created_ptr != 0 {
        println!("[+] Creating Native Window for ANativeActivity onNativeWindowCreated ({:#x})...", on_window_created_ptr);
        let _ = maarch64_thunks::gpu::thunk_XCreateWindow(&mut ctx, &mut mem);
        let win_handle = ctx.get_x(0);

        ctx.pc = on_window_created_ptr;
        ctx.set_x(0, activity_ptr);
        ctx.set_x(1, win_handle);
        ctx.set_x(30, 0);
        let _ = run_cpu_loop(&mut ctx, &mut mem, &thunk_manager, &mut jit_engine, &mut step_count, args.jit);
    }

    println!("[+] Android Native Execution Finished Cleanly (Steps: {})", step_count);
    Ok(())
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
) -> anyhow::Result<()> {
    loop {
        *step_count += 1;
        if *step_count >= 50_000_000 {
            eprintln!("[!] Reached max execution step limit.");
            break;
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

fn extract_apk_native_so(apk_path: &Path) -> anyhow::Result<PathBuf> {
    let file = File::open(apk_path)?;
    let mut archive = ZipArchive::new(file)?;

    let mut so_file_name: Option<String> = None;

    for i in 0..archive.len() {
        let zip_file = archive.by_index(i)?;
        let name = zip_file.name();
        if name.starts_with("lib/arm64-v8a/") && name.ends_with(".so") {
            println!("[+] Found ARM64 NDK shared library in APK: {}", name);
            so_file_name = Some(name.to_string());
            break;
        }
    }

    let target_so_name = so_file_name
        .ok_or_else(|| anyhow::anyhow!("No lib/arm64-v8a/*.so found in APK archive"))?;

    let out_dir = std::env::temp_dir().join("maarch64_apk_extracted");
    fs::create_dir_all(&out_dir)?;

    let mut zip_file = archive.by_name(&target_so_name)?;
    let file_basename = Path::new(&target_so_name)
        .file_name()
        .unwrap_or_default();
    let out_file_path = out_dir.join(file_basename);

    let mut buffer = Vec::new();
    zip_file.read_to_end(&mut buffer)?;
    fs::write(&out_file_path, &buffer)?;

    println!("[+] Extracted native library to: {:?}", out_file_path);
    Ok(out_file_path)
}
