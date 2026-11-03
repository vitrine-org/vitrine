fn main() {
    // Icon, application manifest (PerMonitorV2, Windows 8+ entries) and version info.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_resource::compile("res/vitrine.rc", embed_resource::NONE)
            .manifest_required()
            .expect("unable to compile resources");
    }
    slint_build::compile_with_config(
        "ui/panel.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent".into()),
    )
    .expect("unable to compile the control panel UI");
    println!("cargo:rerun-if-changed=res");
    println!("cargo:rerun-if-changed=../../assets/icon/vitrine.ico");
}
