//! Installing, scanning and packing `.vitrine` widget packages (zip archives).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::manifest::{
    MANIFEST_FILE, PackageManifest, WidgetManifest, is_safe_relative_path, is_valid_package_id,
};
use crate::settings::SettingsSchema;
use crate::{Error, Result};

pub const MAX_FILES: usize = 2000;
pub const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub manifest: PackageManifest,
    pub dir: PathBuf,
}

impl InstalledPackage {
    pub fn widget(&self, id: &str) -> Option<&WidgetManifest> {
        self.manifest.widgets.iter().find(|w| w.id == id)
    }

    /// Absolute path of a package-relative file.
    pub fn resolve(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    /// Loads the widget's settings schema, if it declares one.
    pub fn read_settings_schema(&self, widget: &WidgetManifest) -> Result<Option<SettingsSchema>> {
        let Some(rel) = &widget.settings else { return Ok(None) };
        if !is_safe_relative_path(rel) {
            return Err(Error::Manifest(format!("unsafe settings path `{rel}`")));
        }
        let text = std::fs::read_to_string(self.dir.join(rel))?;
        SettingsSchema::parse(&text).map(Some)
    }
}

fn archive_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Archive(msg.into()))
}

/// Validates an archive entry name and returns it without a trailing `/`.
fn safe_entry_name(name: &str) -> Result<&str> {
    let trimmed = name.strip_suffix('/').unwrap_or(name);
    if !is_safe_relative_path(trimmed) {
        return archive_err(format!("unsafe entry path `{name}`"));
    }
    Ok(trimmed)
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}-{ms:x}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Safely extracts `archive` into `dest` (which must exist and be empty).
fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let mut zip = ZipArchive::new(File::open(archive)?)?;
    if zip.len() > MAX_FILES {
        return archive_err(format!("too many entries ({} > {MAX_FILES})", zip.len()));
    }

    // First pass: validate every entry before writing anything.
    let mut declared = 0u64;
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        let name = entry.name().map_err(|e| Error::Archive(format!("bad entry name: {e}")))?.into_owned();
        safe_entry_name(&name)?;
        if entry.is_symlink() || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return archive_err(format!("symlink entry `{}`", name));
        }
        declared = declared.saturating_add(entry.size());
        if declared > MAX_UNCOMPRESSED_BYTES {
            return archive_err("archive exceeds the uncompressed size limit");
        }
    }

    // Second pass: extract, enforcing the limit on bytes actually written.
    let mut written = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().map_err(|e| Error::Archive(format!("bad entry name: {e}")))?.into_owned();
        let rel = safe_entry_name(&name)?;
        let target = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = match File::create_new(&target) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return archive_err(format!("duplicate entry `{name}`"));
            }
            Err(e) => return Err(e.into()),
        };
        let budget = MAX_UNCOMPRESSED_BYTES - written;
        let n = std::io::copy(&mut (&mut entry).take(budget + 1), &mut out)?;
        written += n;
        if written > MAX_UNCOMPRESSED_BYTES {
            return archive_err("archive exceeds the uncompressed size limit");
        }
    }
    Ok(())
}

/// Reads and validates `vitrine.json` in `dir` and checks every referenced file exists.
fn load_and_verify(dir: &Path) -> Result<PackageManifest> {
    let mpath = dir.join(MANIFEST_FILE);
    let meta = std::fs::metadata(&mpath)
        .map_err(|_| Error::Manifest(format!("{MANIFEST_FILE} not found at package root")))?;
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(Error::Manifest(format!("{MANIFEST_FILE} is too large")));
    }
    let manifest = PackageManifest::parse_validated(&std::fs::read_to_string(&mpath)?)?;
    for rel in manifest.referenced_files() {
        let p = dir.join(rel);
        if !p.is_file() {
            return Err(Error::Manifest(format!("referenced file `{rel}` is missing")));
        }
    }
    Ok(manifest)
}

/// Installs a `.vitrine` archive into `packages_dir/<manifest id>/`,
/// replacing any previous version.
pub fn install(archive: &Path, packages_dir: &Path) -> Result<InstalledPackage> {
    std::fs::create_dir_all(packages_dir)?;
    let tmp = TempDir(packages_dir.join(format!(".install-{}", unique_suffix())));
    std::fs::create_dir(&tmp.0)?;

    extract(archive, &tmp.0)?;
    let manifest = load_and_verify(&tmp.0)?;

    let target = packages_dir.join(&manifest.id);
    if target.exists() {
        let old = TempDir(packages_dir.join(format!(".old-{}", unique_suffix())));
        std::fs::rename(&target, &old.0)?;
        if let Err(e) = std::fs::rename(&tmp.0, &target) {
            let _ = std::fs::rename(&old.0, &target); // roll back
            return Err(e.into());
        }
        // `old` is removed on drop.
    } else {
        std::fs::rename(&tmp.0, &target)?;
    }
    // `tmp` no longer exists; its Drop is a harmless no-op.
    Ok(InstalledPackage { manifest, dir: target })
}

/// Removes `packages_dir/<id>/`. Missing packages are not an error.
pub fn uninstall(id: &str, packages_dir: &Path) -> Result<()> {
    if !is_valid_package_id(id) {
        return Err(Error::Manifest(format!("invalid package id `{id}`")));
    }
    let dir = packages_dir.join(id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Lists installed packages; invalid ones are skipped with a logged reason.
pub fn scan(packages_dir: &Path) -> Vec<InstalledPackage> {
    let Ok(rd) = std::fs::read_dir(packages_dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let dir = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !dir.is_dir() {
            continue;
        }
        match load_and_verify(&dir) {
            Ok(m) if m.id == name => out.push(InstalledPackage { manifest: m, dir }),
            Ok(m) => log::warn!("skipping package dir `{name}`: manifest id is `{}`", m.id),
            Err(e) => log::warn!("skipping package dir `{name}`: {e}"),
        }
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    out
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let ft = e.file_type()?;
        let p = e.path();
        if ft.is_symlink() {
            return archive_err(format!("symlink in package: {}", p.display()));
        }
        if ft.is_dir() {
            collect_files(root, &p, out)?;
        } else if ft.is_file() {
            out.push(p.strip_prefix(root).unwrap_or(&p).to_path_buf());
        }
    }
    Ok(())
}

/// Validates the package folder `src_dir` and zips it into `out_file`.
pub fn pack(src_dir: &Path, out_file: &Path) -> Result<()> {
    let manifest = load_and_verify(src_dir)?;
    let mut files = Vec::new();
    collect_files(src_dir, src_dir, &mut files)?;
    let out_abs = std::fs::canonicalize(out_file).ok();
    files.retain(|rel| std::fs::canonicalize(src_dir.join(rel)).ok() != out_abs || out_abs.is_none());
    if files.len() > MAX_FILES {
        return archive_err(format!("too many files ({} > {MAX_FILES})", files.len()));
    }

    let tmp = out_file.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut zip = ZipWriter::new(File::create(&tmp)?);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let mut total = 0u64;
        // Manifest first.
        files.sort_by_key(|p| p.as_os_str() != MANIFEST_FILE);
        for rel in &files {
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            safe_entry_name(&name)?;
            let mut f = File::open(src_dir.join(rel))?;
            zip.start_file(name, opts)?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            total += buf.len() as u64;
            if total > MAX_UNCOMPRESSED_BYTES {
                return archive_err("package exceeds the uncompressed size limit");
            }
            zip.write_all(&buf)?;
        }
        zip.finish()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            if out_file.exists() {
                std::fs::remove_file(out_file)?;
            }
            std::fs::rename(&tmp, out_file)?;
            log::info!("packed {} ({} files)", manifest.id, files.len());
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_json(id: &str, version: &str) -> String {
        format!(
            r#"{{"id":"{id}","name":"T","version":"{version}","fonts":["assets/f.ttf"],
            "widgets":[{{"id":"clock","name":"Clock","entry":"widgets/clock/index.js",
            "settings":"widgets/clock/settings.json","preview":"widgets/clock/preview.png"}}]}}"#
        )
    }

    fn base_files(id: &str, version: &str) -> Vec<(String, Vec<u8>)> {
        vec![
            ("vitrine.json".into(), manifest_json(id, version).into_bytes()),
            ("widgets/clock/index.js".into(), format!("// {version}").into_bytes()),
            (
                "widgets/clock/settings.json".into(),
                br#"[{"title":"G","settings":{"a":{"type":"boolean","default":true}}}]"#.to_vec(),
            ),
            ("widgets/clock/preview.png".into(), vec![1, 2, 3]),
            ("assets/f.ttf".into(), vec![0; 8]),
        ]
    }

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("vitrine-pkg-{tag}-{}", unique_suffix()));
            std::fs::create_dir_all(&p).unwrap();
            Sandbox(p)
        }
        fn zip(&self, name: &str, files: &[(String, Vec<u8>)]) -> PathBuf {
            let path = self.0.join(name);
            let mut w = ZipWriter::new(File::create(&path).unwrap());
            let o = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            for (n, data) in files {
                w.start_file(n.as_str(), o).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
            path
        }
        fn pkgs(&self) -> PathBuf {
            self.0.join("packages")
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn leftovers(pkgs: &Path) -> Vec<String> {
        std::fs::read_dir(pkgs)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn install_valid_and_upgrade() {
        let sb = Sandbox::new("ok");
        let z1 = sb.zip("whatever-name.vitrine", &base_files("dev.vitrine.t", "1.0.0"));
        let p = install(&z1, &sb.pkgs()).unwrap();
        assert_eq!(p.dir, sb.pkgs().join("dev.vitrine.t"));
        assert!(p.dir.join("widgets/clock/index.js").is_file());
        let w = p.widget("clock").unwrap();
        let schema = p.read_settings_schema(w).unwrap().unwrap();
        assert_eq!(schema.defaults()["a"], serde_json::json!(true));

        // Upgrade replaces the folder: stale files disappear.
        std::fs::write(p.dir.join("stale.txt"), "x").unwrap();
        let z2 = sb.zip("v2.vitrine", &base_files("dev.vitrine.t", "2.0.0"));
        let p2 = install(&z2, &sb.pkgs()).unwrap();
        assert_eq!(p2.manifest.version, "2.0.0");
        assert!(!p2.dir.join("stale.txt").exists());
        assert_eq!(std::fs::read_to_string(p2.dir.join("widgets/clock/index.js")).unwrap(), "// 2.0.0");
        assert!(leftovers(&sb.pkgs()).is_empty());

        let found = scan(&sb.pkgs());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].manifest.id, "dev.vitrine.t");

        uninstall("dev.vitrine.t", &sb.pkgs()).unwrap();
        assert!(scan(&sb.pkgs()).is_empty());
        uninstall("dev.vitrine.t", &sb.pkgs()).unwrap();
        assert!(uninstall("../x", &sb.pkgs()).is_err());
    }

    fn rejects(tag: &str, mutate: impl FnOnce(&mut Vec<(String, Vec<u8>)>)) {
        let sb = Sandbox::new(tag);
        let mut files = base_files("dev.vitrine.t", "1.0.0");
        mutate(&mut files);
        let z = sb.zip("bad.vitrine", &files);
        let err = install(&z, &sb.pkgs()).unwrap_err();
        assert!(matches!(err, Error::Archive(_)), "{tag}: {err}");
        assert!(!sb.pkgs().join("dev.vitrine.t").exists());
        assert!(leftovers(&sb.pkgs()).is_empty(), "{tag}: temp left behind");
    }

    #[test]
    fn rejects_zip_slip() {
        rejects("slip", |f| f.push(("../x".into(), vec![1])));
        rejects("slip2", |f| f.push(("a/../../x".into(), vec![1])));
    }

    #[test]
    fn rejects_absolute_drive_and_backslash() {
        rejects("abs", |f| f.push(("/etc/x".into(), vec![1])));
        rejects("drive", |f| f.push(("C:/x".into(), vec![1])));
        rejects("drive2", |f| f.push(("C:x".into(), vec![1])));
        rejects("bs", |f| f.push(("a\\..\\..\\x".into(), vec![1])));
    }

    #[test]
    fn rejects_symlink() {
        let sb = Sandbox::new("sym");
        let path = sb.0.join("sym.vitrine");
        let mut w = ZipWriter::new(File::create(&path).unwrap());
        let o = SimpleFileOptions::default();
        for (n, d) in base_files("dev.vitrine.t", "1.0.0") {
            w.start_file(n, o).unwrap();
            w.write_all(&d).unwrap();
        }
        w.add_symlink("link", "/etc/passwd", o).unwrap();
        w.finish().unwrap();
        let err = install(&path, &sb.pkgs()).unwrap_err();
        assert!(matches!(err, Error::Archive(ref m) if m.contains("symlink")), "{err}");
        assert!(leftovers(&sb.pkgs()).is_empty());
    }

    #[test]
    fn rejects_oversized_and_too_many() {
        rejects("big", |f| f.push(("big.bin".into(), vec![0u8; 65 * 1024 * 1024])));
        rejects("many", |f| {
            for i in 0..MAX_FILES {
                f.push((format!("f/{i}.txt"), vec![]));
            }
        });
    }

    #[test]
    fn rejects_bad_manifests() {
        let sb = Sandbox::new("man");
        // Missing referenced file.
        let mut files = base_files("dev.vitrine.t", "1.0.0");
        files.retain(|(n, _)| n != "widgets/clock/preview.png");
        let z = sb.zip("a.vitrine", &files);
        assert!(matches!(install(&z, &sb.pkgs()), Err(Error::Manifest(_))));
        // No manifest.
        let z = sb.zip("b.vitrine", &[("x.txt".into(), vec![1])]);
        assert!(matches!(install(&z, &sb.pkgs()), Err(Error::Manifest(_))));
        // Malicious id can never become a directory name.
        let z = sb.zip("c.vitrine", &base_files("../evil", "1.0.0"));
        assert!(install(&z, &sb.pkgs()).is_err());
        assert!(leftovers(&sb.pkgs()).is_empty());
        assert!(scan(&sb.pkgs()).is_empty());
    }

    #[test]
    fn pack_round_trip() {
        let sb = Sandbox::new("pack");
        let src = sb.0.join("src");
        for (n, d) in base_files("dev.vitrine.t", "1.0.0") {
            let p = src.join(&n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, d).unwrap();
        }
        let out = sb.0.join("out.vitrine");
        pack(&src, &out).unwrap();
        let p = install(&out, &sb.pkgs()).unwrap();
        assert_eq!(p.manifest.id, "dev.vitrine.t");
        std::fs::remove_file(src.join("assets/f.ttf")).unwrap();
        assert!(pack(&src, &sb.0.join("o2.vitrine")).is_err());
    }
}
