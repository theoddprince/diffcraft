//! Embeds the DiffCraft icon and version information in the Windows executables.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/brand/diffcraft.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/brand/diffcraft.ico");
        res.set("ProductName", "DiffCraft");
        res.set("FileDescription", "DiffCraft - compare and merge files, folders, binaries and pictures");
        res.set("LegalCopyright", "The DiffCraft contributors. MIT OR Apache-2.0");
        if let Err(e) = res.compile() {
            // A missing resource compiler only loses the icon; never fail the build for it.
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
