// SPDX-License-Identifier: GPL-3.0-or-later
//! The arguments the desktop hands the launcher.
//!
//! It reads two flags — `--version`/`-V` and `--hidden` — and ignores everything else, because a
//! desktop start-up brings along what it brings along: `-psn_0_…` from Finder when a bundle is
//! opened, a file dropped on the icon, an old login entry's leftovers. An argument the launcher
//! cannot read is ignored in the same way, rather than ending the start-up before the window.
//!
//! `--version` is the only thing the launcher does without opening a window, so it is what a test
//! can ask for from here.

use std::process::Command;

fn launcher() -> Command {
    Command::new(env!("CARGO_BIN_EXE_open3d-launcher"))
}

const VERSION_LINE: &str = concat!("open3d-launcher ", env!("CARGO_PKG_VERSION"), "\n");

#[test]
fn the_version_is_printed_with_arguments_the_desktop_adds() {
    for extra in [vec![], vec!["-psn_0_1234567"], vec!["/tmp/a file.png"]] {
        for flag in ["--version", "-V"] {
            let mut args = extra.clone();
            args.push(flag);
            let out = launcher().args(&args).output().unwrap_or_else(|e| panic!("{args:?}: {e}"));
            let err = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(0), "{args:?}: {err}");
            assert_eq!(String::from_utf8_lossy(&out.stdout), VERSION_LINE, "{args:?}");
        }
    }
}
