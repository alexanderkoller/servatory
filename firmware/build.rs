include!("../build-support.rs");

#[path = "build-checks.rs"]
mod build_checks;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR");
    let repository = std::path::Path::new(&manifest_dir)
        .parent()
        .expect("firmware crate is inside the repository");
    let source = std::fs::read_to_string(repository.join("firmware/src/network.rs"))
        .expect("reading firmware/src/network.rs");
    let css = std::fs::read(repository.join("firmware/src/dashboard.css"))
        .expect("reading firmware/src/dashboard.css");
    let javascript = std::fs::read(repository.join("firmware/src/dashboard.js"))
        .expect("reading firmware/src/dashboard.js");
    build_checks::validate_dashboard(&source, &css, &javascript)
        .unwrap_or_else(|error| panic!("firmware dashboard safety check failed: {error}"));
    emit_build_version(&[
        "firmware/src",
        "firmware/.cargo/config.toml",
        "firmware/Cargo.toml",
    ]);
}
