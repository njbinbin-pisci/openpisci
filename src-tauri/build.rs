fn main() {
    #[cfg(target_os = "linux")]
    build_xi_helper();

    #[cfg(target_os = "windows")]
    {
        let mut attributes = tauri_build::Attributes::new();
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        add_windows_manifest();
        tauri_build::try_build(attributes).expect("failed to run tauri build");
    }

    #[cfg(not(target_os = "windows"))]
    tauri_build::build()
}

/// Build the pisci-xi-helper C program that uses XIWarpPointer for mouse positioning.
/// This is needed in VMware+Xorg where xdotool mousemove only updates the XTEST
/// slave pointer, not the XInput2 master pointer that applications see.
#[cfg(target_os = "linux")]
fn build_xi_helper() {
    let src = std::path::Path::new("xi_helpers.c");
    println!("cargo:rerun-if-changed={}", src.display());

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_path = std::path::Path::new(&out_dir).join("pisci-xi-helper");

    let status = std::process::Command::new("gcc")
        .args([
            src.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "-lX11",
            "-lXi",
            "-O2",
        ])
        .status()
        .expect("failed to run gcc for xi_helpers.c");

    if !status.success() {
        // Not fatal — xdotool fallback will be used
        eprintln!(
            "WARNING: Failed to build pisci-xi-helper (gcc exit {:?}). \
                  Xdotool fallback will be used for mouse positioning.",
            status.code()
        );
    }
}

#[cfg(target_os = "windows")]
fn add_windows_manifest() {
    let manifest = std::env::current_dir()
        .expect("build.rs cwd")
        .join("windows-app-manifest.xml");

    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    println!("cargo:rustc-link-arg=/WX");
}
