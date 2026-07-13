use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const MANIFEST_NAME: &str = "rustreplay-embedded-vpl-manifest.rs";
const DISPATCHER_NAME: &str = "libvpl-2.dll";
const OPTIONAL_RUNTIME_DEPENDENCIES: &[&str] = &[
    "libgcc_s_seh-1.dll",
    "libstdc++-6.dll",
    "libwinpthread-1.dll",
];

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTREPLAY_VPL_DLL");

    let source = find_vpl_dll().unwrap_or_else(|| {
        panic!(
            "oneVPL dispatcher DLL not found; set RUSTREPLAY_VPL_DLL to libvpl-2.dll/libvpl.dll before building"
        )
    });
    println!("cargo:rerun-if-changed={}", source.display());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let mut runtime_files = vec![(DISPATCHER_NAME, source.clone())];
    if let Some(source_dir) = source.parent() {
        for name in OPTIONAL_RUNTIME_DEPENDENCIES {
            let dependency = source_dir.join(name);
            if is_regular_file(&dependency) {
                runtime_files.push((name, dependency));
            }
        }
    }

    let mut manifest = String::from("const EMBEDDED_VPL_FILES: &[EmbeddedVplFile] = &[\n");
    for (name, source_path) in runtime_files {
        println!("cargo:rerun-if-changed={}", source_path.display());
        let destination = out_dir.join(name);
        fs::copy(&source_path, &destination).unwrap_or_else(|err| {
            panic!(
                "failed to stage embedded runtime {} -> {}: {err}",
                source_path.display(),
                destination.display()
            )
        });
        manifest.push_str(&format!(
            "    EmbeddedVplFile {{ file_name: {name:?}, bytes: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{name}\")) }},\n"
        ));
    }
    manifest.push_str("];\n");
    fs::write(out_dir.join(MANIFEST_NAME), manifest)
        .expect("failed to generate embedded oneVPL runtime manifest");
}

fn find_vpl_dll() -> Option<PathBuf> {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"));
    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("RUSTREPLAY_VPL_DLL") {
        candidates.push(PathBuf::from(path));
    }
    candidates.extend([
        manifest_dir.join("libvpl-2.dll"),
        manifest_dir.join("libvpl.dll"),
        PathBuf::from(r"C:\Program Files (x86)\Intel\oneAPI\vpl\latest\bin\libvpl.dll"),
        PathBuf::from(r"C:\Program Files\Intel\oneAPI\vpl\latest\bin\libvpl.dll"),
        PathBuf::from(r"C:\msys64\ucrt64\bin\libvpl-2.dll"),
    ]);
    candidates.into_iter().find(|path| is_regular_file(path))
}

fn is_regular_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|metadata| metadata.is_file())
}
