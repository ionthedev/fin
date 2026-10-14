fn main() {
    #[cfg(feature = "desktop")]
    build_desktop();
}

#[cfg(feature = "desktop")]
fn build_desktop() {
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        // Slint's testing API needs element metadata for real pointer/key tests.
        .with_debug_info(true);
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("Could not compile the Slint UI");
}
