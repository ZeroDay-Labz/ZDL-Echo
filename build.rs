use std::io;

// This function ONLY exists when compiling on Windows
#[cfg(windows)]
fn main() -> io::Result<()> {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/zdl-echo.ico");
    res.compile()?;
    Ok(())
}

// This function ONLY exists when compiling on Mac or Linux
#[cfg(not(windows))]
fn main() -> io::Result<()> {
    Ok(())
}