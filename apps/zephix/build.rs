//! Embeds the icon and version information (product name, author) into the Windows `.exe`.

fn main() {
    println!("cargo:rerun-if-changed=assets/zephix.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/zephix.ico")
            .set("ProductName", "Zephix - Database Management Studio")
            .set("FileDescription", "Zephix - Database Management Studio")
            .set("CompanyName", "Enrique Muhammad Ilham")
            .set(
                "LegalCopyright",
                "Copyright (c) 2026 Enrique Muhammad Ilham. Author and creator of Zephix.",
            )
            .set("OriginalFilename", "zephix.exe")
            .set("InternalName", "zephix");
        if let Err(e) = res.compile() {
            // Missing Windows SDK resource compiler: build without icon/metadata.
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
