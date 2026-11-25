//! Windows gives the main thread a 1 MiB stack by default (Linux and macOS
//! give 8 MiB). Reserve 8 MiB for the `cpf` binary on Windows as well, so
//! unoptimised builds have the same headroom on every platform. Only the
//! binary target is affected; library and test executables keep their defaults.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    const STACK: &str = "8388608";
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bin=cpf=/STACK:{STACK}"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bin=cpf=-Wl,--stack,{STACK}"),
        _ => {}
    }
}
