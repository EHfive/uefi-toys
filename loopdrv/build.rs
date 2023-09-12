fn main() {
    println!("cargo:rustc-link-arg=/subsystem:EFI_BOOT_SERVICE_DRIVER");
    println!("cargo:rerun-if-changed=build.rs");
}
