//! Put user.ld where the linker will find it for every program using
//! libpios (they link with `-Tuser.ld`, see user/.cargo/config.toml).

use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::copy("user.ld", out.join("user.ld")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=user.ld");
}
