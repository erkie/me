//! On Windows, puts the app icon and name in me-app.exe: File Explorer and
//! the taskbar show the icon (GPUI loads it for the window too), and Task
//! Manager shows "Me" rather than the crate's description.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        let icon = "../../assets/logo/app-icon.ico";
        println!("cargo:rerun-if-changed={icon}");
        winresource::WindowsResource::new()
            .set_icon(icon)
            .set("ProductName", "Me")
            .set("FileDescription", "Me")
            .compile()
            .expect("could not embed the app icon");
    }
}
