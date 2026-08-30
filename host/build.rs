include!("../build-support.rs");

fn main() {
    emit_build_version(&["host/src", "host/Cargo.toml"]);
}
