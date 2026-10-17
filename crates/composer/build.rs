use std::{fmt::Write, path::Path};
fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    if std::env::var_os("CARGO_FEATURE_NATIVE").is_none() {
        return;
    }
    let root =
        std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("web/dist");
    assert!(
        root.join("index.html").exists(),
        "Build the local editor first: cd crates/composer/web && npm ci && npm run build"
    );
    let mut source = String::from(
        "fn asset(path: &str) -> Option<(&'static [u8], &'static str)> { match path {\n",
    );
    fn walk(root: &Path, dir: &Path, code: &mut String) {
        let mut entries = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(root, &path, code);
                continue;
            }
            let relative = path.strip_prefix(root).unwrap().to_string_lossy();
            let mime = match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
                "js" => "text/javascript",
                "css" => "text/css",
                "html" => "text/html",
                "svg" => "image/svg+xml",
                "ttf" => "font/ttf",
                _ => "application/octet-stream",
            };
            writeln!(
                code,
                "{:?} => Some((include_bytes!({:?}), {:?})),",
                format!("/{relative}"),
                path,
                mime
            )
            .unwrap();
        }
    }
    walk(&root, &root, &mut source);
    source.push_str("_ => None, } }\n");
    std::fs::write(
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("assets.rs"),
        source,
    )
    .unwrap();
}
