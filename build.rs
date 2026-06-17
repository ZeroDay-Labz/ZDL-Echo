use std::io;

fn main() -> io::Result<()> {
    // Only run this resource compiler if we are building for Windows
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        let mut res = winres::WindowsResource::new();
        // Point this to your generated icon
        res.set_icon("assets/zdl-echo.ico");
        res.compile()?;
    }
    Ok(())
}