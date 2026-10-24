fn main() {
    // The exe and window icon; platform.rs loads it as ordinal 1.
    #[cfg(windows)]
    winresource::WindowsResource::new().set_icon("assets/icons/vjuu.ico").compile().expect("embed the Windows icon");
}
