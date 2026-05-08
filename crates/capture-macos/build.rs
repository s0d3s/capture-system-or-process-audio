use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/macos_bridge.m");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_NATIVE_TESTS");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" || env::var_os("DOCS_RS").is_some() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let obj_path = out_dir.join("macos_bridge.o");
    let lib_path = out_dir.join("libcspa_macos_bridge.a");

    let mut clang = Command::new("clang");
    clang.args([
        "-x",
        "objective-c",
        "-std=c11",
        "-fobjc-arc",
        "-fPIC",
        "-Wall",
        "-Wextra",
        "-Werror",
    ]);
    if env::var("CARGO_FEATURE_NATIVE_TESTS").is_ok() {
        clang.arg("-DCSPA_NATIVE_TESTS=1");
    }
    let status = clang
        .args(["-c", "src/macos_bridge.m", "-o"])
        .arg(&obj_path)
        .status()
        .expect("invoke clang for macos_bridge.m");
    assert!(status.success(), "clang failed to compile macos_bridge.m");

    let ar_status = Command::new("ar")
        .args(["rcs"])
        .arg(&lib_path)
        .arg(&obj_path)
        .status()
        .expect("invoke ar for libcspa_macos_bridge.a");
    assert!(
        ar_status.success(),
        "ar failed to archive libcspa_macos_bridge.a"
    );

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=cspa_macos_bridge");
    for fw in [
        "Foundation",
        "CoreFoundation",
        "CoreAudio",
        "AudioToolbox",
        "AVFoundation",
        "ScreenCaptureKit",
        "ApplicationServices",
        "CoreMedia",
        "AppKit",
    ] {
        println!("cargo:rustc-link-lib=framework={fw}");
    }
}
