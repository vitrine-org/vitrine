fn main() {
    // Icon, application manifest (PerMonitorV2, Windows 8+ entries) and version info.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_resource::compile("res/vitrine.rc", embed_resource::NONE)
            .manifest_required()
            .expect("unable to compile resources");
    }
    let translations = write_translations();
    slint_build::compile_with_config(
        "ui/panel.slint",
        slint_build::CompilerConfiguration::new()
            .with_style("fluent".into())
            .with_bundled_translations(translations)
            .with_default_translation_context(slint_build::DefaultTranslationContext::None),
    )
    .expect("unable to compile the control panel UI");
    // Widget scripts run on the main thread; QuickJS needs more than the
    // default 1 MB stack (its own limit is 1 MB of script stack on top).
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
    println!("cargo:rerun-if-changed=res");
    println!("cargo:rerun-if-changed=locales");
    println!("cargo:rerun-if-changed=../../assets/icon/vitrine.ico");
}

/// Turns the `panel.*` strings of `locales/<lang>.json` into the gettext
/// catalogues Slint bundles: the English text is the `@tr()` source string.
fn write_translations() -> std::path::PathBuf {
    use std::collections::BTreeMap;
    type Table = BTreeMap<String, String>;
    let read = |lang: &str| -> Table {
        let path = format!("locales/{lang}.json");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
    };
    let po = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let en = read("en");
    let root = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("translations");
    let domain = std::env::var("CARGO_PKG_NAME").unwrap();
    for entry in std::fs::read_dir("locales").unwrap() {
        let path = entry.unwrap().path();
        let lang = path.file_stem().unwrap().to_string_lossy().to_string();
        if lang == "en" {
            continue;
        }
        let table = read(&lang);
        let mut out = String::from(
            "msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\n",
        );
        for (key, source) in en.iter().filter(|(k, _)| k.starts_with("panel.")) {
            let text = table.get(key).unwrap_or(source);
            out.push_str(&format!(
                "msgid \"{}\"\nmsgstr \"{}\"\n\n",
                po(source),
                po(text)
            ));
        }
        let dir = root.join(&lang).join("LC_MESSAGES");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{domain}.po")), out).unwrap();
    }
    root
}
