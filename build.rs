use std::{env::var, fs, path::PathBuf};

pub fn main() {
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    // 获取 crate 根路径
    let crate_dir = var("CARGO_MANIFEST_DIR").unwrap();
    let include_dir = PathBuf::from(&crate_dir).join("include");
    if !include_dir.exists() {
        fs::create_dir_all(&include_dir).expect("Failed to create include directory");
    }

    let header_path = include_dir.join("syncdaq.h");

    cbindgen::Builder::new()
        .with_crate(crate_dir)
        .with_config(cbindgen::Config::from_file("cbindgen.toml").unwrap())
        .generate()
        .unwrap()
        .write_to_file(header_path);
}
