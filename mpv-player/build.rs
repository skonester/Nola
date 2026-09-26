use std::env;
use std::path::PathBuf;

// Link against the same libmpv the app ships (src-tauri/libs/mpv), so this
// player adds only a small executable on top of the bundled runtime.
fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let mpv_lib_dir = manifest_dir.join("..").join("src-tauri").join("libs").join("mpv");
    if !mpv_lib_dir.is_dir() {
        panic!(
            "\n[!] Cannot find the libmpv runtime at {}. Please run: npm run setup:libs\n",
            mpv_lib_dir.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", mpv_lib_dir.display());
    println!("cargo:rustc-link-lib=mpv");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib");
    }
    println!("cargo:rerun-if-changed=../src-tauri/libs/mpv");
}
