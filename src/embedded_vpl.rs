use crate::config::AppConfig;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

struct EmbeddedVplFile {
    file_name: &'static str,
    bytes: &'static [u8],
}

include!(concat!(
    env!("OUT_DIR"),
    "/rustreplay-embedded-vpl-manifest.rs"
));

pub fn ensure_installed() -> Result<PathBuf, String> {
    let directory = AppConfig::config_dir();
    for file in EMBEDDED_VPL_FILES {
        ensure_file(&directory.join(file.file_name), file.bytes)?;
    }
    Ok(AppConfig::vpl_dll_path())
}

fn ensure_file(path: &Path, expected: &[u8]) -> Result<(), String> {
    if file_matches(path, expected) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("oneVPL DLL 路径没有父目录：{}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("创建配置目录 {} 失败：{err}", parent.display()))?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("libvpl-2.dll");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let write_result = (|| -> Result<(), String> {
        let mut file = fs::File::create(&temporary)
            .map_err(|err| format!("创建临时 oneVPL DLL {} 失败：{err}", temporary.display()))?;
        file.write_all(expected)
            .map_err(|err| format!("写入临时 oneVPL DLL {} 失败：{err}", temporary.display()))?;
        file.sync_all()
            .map_err(|err| format!("同步临时 oneVPL DLL {} 失败：{err}", temporary.display()))?;
        if path.exists() {
            fs::remove_file(path)
                .map_err(|err| format!("替换旧 oneVPL DLL {} 失败：{err}", path.display()))?;
        }
        fs::rename(&temporary, path).map_err(|err| {
            format!(
                "安装内嵌 oneVPL DLL {} -> {} 失败：{err}",
                temporary.display(),
                path.display()
            )
        })?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn file_matches(path: &Path, expected: &[u8]) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == expected.len() as u64)
        && fs::read(path).is_ok_and(|current| current == expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn embedded_runtime_files_are_pe_images() {
        assert!(!EMBEDDED_VPL_FILES.is_empty());
        for file in EMBEDDED_VPL_FILES {
            assert!(file.bytes.len() > 50_000, "{} is too small", file.file_name);
            assert_eq!(
                &file.bytes[..2],
                b"MZ",
                "{} is not a PE file",
                file.file_name
            );
        }
    }

    #[test]
    fn install_repairs_a_changed_file() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rustreplay-embedded-vpl-{}-{nonce}",
            std::process::id()
        ));
        let path = root.join("libvpl-2.dll");
        let dispatcher = EMBEDDED_VPL_FILES
            .iter()
            .find(|file| file.file_name == "libvpl-2.dll")
            .unwrap()
            .bytes;

        ensure_file(&path, dispatcher).unwrap();
        assert!(file_matches(&path, dispatcher));
        fs::write(&path, b"changed").unwrap();
        ensure_file(&path, dispatcher).unwrap();
        assert!(file_matches(&path, dispatcher));

        fs::remove_dir_all(root).unwrap();
    }
}
