//! Expone el target triple real de la compilación para el handshake
//! `engine_ready`, en vez de anunciar siempre x86_64 Linux.

fn main() {
    let target = std::env::var("TARGET").expect("cargo always sets TARGET");
    println!("cargo:rustc-env=AGENTRIX_BUILD_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
