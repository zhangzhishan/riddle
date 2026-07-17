fn main() {
    if std::env::var("CARGO_FEATURE_TAKEOVER").is_ok() {
        // libquill.so + libqsgepaper.so from the quill project.
        let quill = concat!(env!("CARGO_MANIFEST_DIR"), "/../quill");
        println!("cargo:rustc-link-search=native={quill}/build");
        println!("cargo:rustc-link-search=native={quill}/vendor");
        println!("cargo:rustc-link-lib=dylib=quill");
        println!("cargo:rustc-link-lib=dylib=qsgepaper");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/home/root/quill:/usr/lib/plugins/scenegraph");
        // Resolve libquill's transitive Qt deps at link time from the SDK
        // sysroot. rpath-link only (NOT link-search: the SDK's libc/libm are
        // linker scripts with absolute paths that break outside --sysroot).
        if let Ok(home) = std::env::var("HOME") {
            let sysroot =
                format!("{home}/rm-sdk-3.26/sysroots/cortexa53-crypto-remarkable-linux/usr/lib");
            println!("cargo:rustc-link-arg=-Wl,-rpath-link,{sysroot}");
        }
    }

    let is_kobo = std::env::var("CARGO_FEATURE_KOBO").is_ok();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if is_kobo && target_os == "linux" {
        let fbink = std::env::var("RIDDLE_FBINK_DIR").unwrap_or_else(|_| {
            panic!("RIDDLE_FBINK_DIR must point to a built FBInk checkout for --features kobo")
        });
        println!("cargo:rerun-if-changed=native/kobo_fbink_shim.c");
        println!("cargo:rerun-if-changed=native/kobo_fbink_shim.h");
        println!("cargo:rerun-if-env-changed=RIDDLE_FBINK_DIR");
        cc::Build::new()
            .file("native/kobo_fbink_shim.c")
            .include("native")
            .include(&fbink)
            .define("FBINK_FOR_KOBO", None)
            .warnings(true)
            .compile("riddle_kobo_fbink_shim");
        println!("cargo:rustc-link-search=native={fbink}/Release");
        println!("cargo:rustc-link-lib=static=fbink");
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=rt");
    }
}
