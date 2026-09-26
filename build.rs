//! Pass our linker script to the linker, build the user programs, and pack
//! them into the boot image the kernel carries (see docs/design.md).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The user programs in the boot image; see src/bootimage.rs.
const PROGRAMS: &[&str] = &[
    "init",
    "console",
    "procman",
    "shell",
    "usb",
    "hello",
    "usertest",
    "crashtest",
    "fptest",
    "ipctest",
];

fn main() {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rustc-link-arg-bins=-T{}/linker.ld", dir.display());
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=src/boot.s");
    build_user_programs(&dir);
}

/// Build the `user/` workspace for EL0 and pack the programs into
/// OUT_DIR/boot.img, for `include_bytes!`.
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

    let programs: Vec<(&str, Vec<u8>)> = PROGRAMS
        .iter()
        .map(|&program| {
            let built = target_dir
                .join("aarch64-unknown-none/release")
                .join(program);
            let elf = std::fs::read(&built)
                .unwrap_or_else(|e| panic!("reading {}: {e}", built.display()));
            (program, elf)
        })
        .collect();
    let files: Vec<(&str, &[u8])> = programs
        .iter()
        .map(|(name, elf)| (*name, elf.as_slice()))
        .collect();
    let image = pios_bootfs::build(&files).expect("the programs make a valid boot image");
    std::fs::write(out.join("boot.img"), image).expect("writing the boot image");

    for path in [
        "user/Cargo.toml",
        "user/.cargo",
        "user/libpios",
        "abi",
        "bootfs",
        "pl011",
        "textconsole",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    for program in PROGRAMS {
        println!("cargo:rerun-if-changed=user/{program}");
    }
}
