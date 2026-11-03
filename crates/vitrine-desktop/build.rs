use embed_manifest::manifest::{DpiAwareness, SupportedOS};
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    // Child layered windows (one per widget) require the Windows 8+
    // compatibility entries; per-monitor v2 DPI awareness keeps the grid crisp.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(
            new_manifest("Vitrine")
                .supported_os(SupportedOS::Windows7..=SupportedOS::Windows10)
                .dpi_awareness(DpiAwareness::PerMonitorV2),
        )
        .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
