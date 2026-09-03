use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=windows/app.manifest");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("缺少项目目录"))
            .join("windows")
            .join("app.manifest");
        for binary in ["clipx", "clipx-gui"] {
            println!("cargo:rustc-link-arg-bin={binary}=/MANIFEST:EMBED");
            println!(
                "cargo:rustc-link-arg-bin={binary}=/MANIFESTINPUT:{}",
                manifest.display()
            );
        }
    }
}
