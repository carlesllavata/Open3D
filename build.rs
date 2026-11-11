// SPDX-License-Identifier: GPL-3.0-or-later
//! Embeds the icon and version details in the Windows exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("ProductName", "Open3D Launcher")
            .set("FileDescription", "Open3D Launcher")
            .set("CompanyName", "Open3D")
            .set(
                "LegalCopyright",
                "Copyright (C) 2026 Carles Llavata and Open3D contributors. License: GPL-3.0-or-later",
            );
        if let Err(e) = res.compile() {
            panic!("Could not embed the Windows resources (icon, version): {e}");
        }
    }
}
