fn main() {
    // `slint-build` parses the declarative UI at compile time and writes Rust
    // bindings into Cargo's `OUT_DIR`. `slint::include_modules!()` in main.rs
    // later includes those generated types, so an invalid UI fails the build
    // before the application can start.
    slint_build::compile("ui/main.slint").expect("Failed to compile Slint UI");
}
