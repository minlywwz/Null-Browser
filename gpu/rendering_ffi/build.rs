use std::path::Path;

fn main() {
    let native = Path::new("../rendering_engine");
    println!("cargo:rerun-if-changed={}", native.display());
    assert_eq!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("windows")
    );
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .define("WIN32_LEAN_AND_MEAN", None)
        .define("NOMINMAX", None)
        .include(native.join("include"))
        .file(native.join("src/renderer.cpp"))
        .file(native.join("src/text.cpp"))
        .file(native.join("src/motion_engine/motion_engine.cpp"))
        .file(native.join("src/media_player.cpp"))
        .compile("rendering_engine");
    for library in [
        "d3d11",
        "dxgi",
        "d2d1",
        "dwrite",
        "gdi32",
        "ole32",
        "mfplat",
        "mfuuid",
        "mfreadwrite",
    ] {
        println!("cargo:rustc-link-lib={library}");
    }
}
