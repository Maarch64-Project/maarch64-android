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

    loop {
        step_count += 1;
        if step_count >= 50_000_000 {
            eprintln!("[!] Reached max execution step limit.");
            break;
        }

        if let Some(thunk) = thunk_manager.get_thunk_by_address(ctx.pc) {
            let entry_pc = ctx.pc;
            let _ = thunk(&mut ctx, &mut mem);
            if ctx.exited {
                break;
            }
            if ctx.pc == entry_pc {
                ctx.pc = ctx.get_x(30);
            }
            continue;
        }

        if ctx.pc == 0 {
            tracing::info!("[+] Function returned to NULL (PC=0), finishing entry point execution.");
            break;
        }

        let res = if args.jit {
            jit_engine.execute(&mut ctx, &mut mem)
        } else {
            Interpreter::step(&mut ctx, &mut mem)
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

    println!("[+] Android Native Execution Finished Cleanly (Steps: {})", step_count);
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
