use std::{collections::BTreeMap, path::PathBuf};
fn main() {
    println!("cargo:rerun-if-changed=i18n/fr.json");
    let translations: BTreeMap<String, String> =
        serde_json::from_str(include_str!("i18n/fr.json")).unwrap();
    let root = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("translations");
    let dir = root.join("fr/LC_MESSAGES");
    std::fs::create_dir_all(&dir).unwrap();
    let mut po = String::from("msgid \"\"\nmsgstr \"\"\n\"Content-Type: text/plain; charset=UTF-8\\n\"\n\"Language: fr\\n\"\n\"Plural-Forms: nplurals=2; plural=(n > 1);\\n\"\n\n");
    for (key, value) in translations {
        po.push_str(&format!(
            "msgid {}\nmsgstr {}\n\n",
            serde_json::to_string(&key).unwrap(),
            serde_json::to_string(&value).unwrap()
        ));
    }
    po.push_str("msgid \"{n} message\"\nmsgid_plural \"{n} messages\"\nmsgstr[0] \"{n} message\"\nmsgstr[1] \"{n} messages\"\n");
    std::fs::write(dir.join("app.po"), po).unwrap();
    let config = slint_build::CompilerConfiguration::new()
        .with_style("cupertino".into())
        .with_debug_info(std::env::var_os("CARGO_FEATURE_PREVIEW_RENDERER").is_some())
        .with_default_translation_context(slint_build::DefaultTranslationContext::None)
        .with_bundled_translations(root);
    slint_build::compile_with_config("ui/app.slint", config).unwrap();
}
