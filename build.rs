//! Pass our linker script to the linker, and build the user programs the
//! kernel carries (until they come from an initramfs).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// User programs embedded in the kernel; see src/process.rs.
const PROGRAMS: &[&str] = &["hello", "usertest", "crashtest", "fptest"];

fn main() {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rustc-link-arg-bins=-T{}/linker.ld", dir.display());
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=src/boot.s");
    build_user_programs(&dir);
}

/// Build the `user/` workspace for EL0 and copy each program's ELF file to
/// OUT_DIR, for `include_bytes!`.
fn build_user_programs(dir: &Path) {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target_dir = out.join("user-target");
    let mut cargo = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo
        .current_dir(dir.join("user"))
        .args(["build", "--release", "--target-dir"])
        .arg(&target_dir);
    // The kernel build's settings (target, flags) must not leak into the
    // user programs' build, which has its own (user/.cargo/config.toml).
    for (key, _) in env::vars() {
        if (key.starts_with("CARGO_") && key != "CARGO_HOME")
            || key.starts_with("RUSTFLAGS")
            || key == "RUSTDOCFLAGS"
        {
            cargo.env_remove(key);
        }
    }
    let status = cargo
        .status()
        .expect("could not run cargo to build the user programs");
    assert!(
        status.success(),
        "building the user programs (in user/) failed"
    );

    for program in PROGRAMS {
        let built = target_dir
            .join("aarch64-unknown-none/release")
            .join(program);
        std::fs::copy(&built, out.join(format!("{program}.elf")))
            .unwrap_or_else(|e| panic!("copying {}: {e}", built.display()));
    }
    for path in ["user/Cargo.toml", "user/.cargo", "user/libpios", "abi"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for program in PROGRAMS {
        println!("cargo:rerun-if-changed=user/{program}");
    }
}
