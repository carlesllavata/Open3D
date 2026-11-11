// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows only: embed the app icon and version info (VERSIONINFO) into `open3d-launcher.exe`.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, unless
//! `OPEN3D_LAUNCHER_REQUIRE_WINRES=1` (set by the release packaging) turns it into an error.
//!
//! Based on ArtCraft Launcher's build script (MIT OR Apache-2.0, see NOTICE).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/open3d/icon.ico");
    println!("cargo:rerun-if-env-changed=OPEN3D_LAUNCHER_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/open3d/icon.ico")
        .set("ProductName", "Open3D Launcher")
        .set("FileDescription", "Open3D Launcher")
        .set("CompanyName", "Open3D")
        .set(
            "LegalCopyright",
            "Copyright (c) 2026 Carles Llavata and Open3D contributors. Based on ArtCraft Launcher, \
             Copyright (c) 2026 ArtCraft Team and the ArtCraft Launcher contributors. GPL-3.0-or-later.",
        )
        .set("OriginalFilename", "open3d-launcher.exe")
        .set("InternalName", "open3d-launcher");
    if let Err(e) = res.compile() {
        if std::env::var_os("OPEN3D_LAUNCHER_REQUIRE_WINRES").is_some() {
            println!("cargo:warning=embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=open3d-launcher.exe built without icon/version resources: {e}");
    }
}
