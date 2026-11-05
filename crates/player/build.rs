fn main() {
    // libmpv2-sys emits `rustc-link-lib=mpv`. On Windows/MSVC that needs `mpv.lib`, which is
    // not on the default search path, so MPV_LIB_DIR points at it (see README.md).
    println!("cargo:rerun-if-env-changed=MPV_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_MPV").is_some() {
        if let Some(dir) = std::env::var_os("MPV_LIB_DIR") {
            println!(
                "cargo:rustc-link-search=native={}",
                std::path::Path::new(&dir).display()
            );
        }
    }
}
