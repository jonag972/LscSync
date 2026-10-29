//! Windows: embeds the app icon in the `.exe` (Explorer, taskbar, Alt+Tab).
//! Nothing to do on other platforms.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // Repository root without `..` or canonicalize (which yields `\\?\C:\…`,
    // rejected by rc.exe).
    let root = manifest_dir.parent().and_then(|p| p.parent()).unwrap();
    let icon = root.join("assets").join("icon").join("lscsync.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Resource 1: GPUI loads it as the window and taskbar icon.
    // Absolute path: the .rc is generated in OUT_DIR.
    let rc = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app.rc");
    let icon_path = icon.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{icon_path}\"\n")).expect("writing app.rc");
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .expect("compiling the Windows icon");
}
