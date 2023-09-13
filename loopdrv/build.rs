use std::env::var;

fn main() {
    let target = var("TARGET").unwrap();
    if target.contains("uefi") {
        println!("cargo:rustc-link-arg=/subsystem:EFI_BOOT_SERVICE_DRIVER");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=/null");
}
