include!("../build-support.rs");

fn main() {
    emit_build_version(&[
        "firmware/src",
        "firmware/.cargo/config.toml",
        "firmware/Cargo.toml",
    ]);
}
