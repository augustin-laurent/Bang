//! Bang desktop mail client — entry point.

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let ui = BangApp::new()?;
    ui.run()
}
