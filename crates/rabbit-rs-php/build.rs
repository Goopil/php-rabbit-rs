use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set"));
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "macos" {
        let map = manifest_dir.join("export.macos");
        println!(
            "cargo:rustc-link-arg-cdylib=-Wl,-exported_symbols_list,{}",
            map.display()
        );
    }

    println!("cargo:rerun-if-changed=export.macos");
}
