//! Geometry kernel binary resolution, Python/PyO3 environment detection,
//! and OCC cache directory management for the topology-full pipeline.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
#[derive(Debug)]
pub(crate) struct CacheDirGuard {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl Drop for CacheDirGuard {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            if let Err(error) = std::fs::remove_dir_all(&self.path) {
                tracing::warn!(
                    "failed to remove temporary OCC cache dir {}: {}",
                    self.path.display(),
                    error
                );
            }
        }
    }
}

pub(crate) fn prepare_kernel_cache_args(
    input_path: &Path,
) -> anyhow::Result<(Vec<String>, CacheDirGuard)> {
    if let Ok(override_dir) = std::env::var("IFC2LBD_OCC_CACHE_DIR") {
        let path = PathBuf::from(override_dir);
        std::fs::create_dir_all(&path).with_context(|| {
            format!(
                "failed to create IFC2LBD_OCC_CACHE_DIR at {}",
                path.display()
            )
        })?;
        return Ok((
            vec![
                "--brep-cache-dir".to_string(),
                path.to_string_lossy().into_owned(),
            ],
            CacheDirGuard {
                path,
                cleanup_on_drop: false,
            },
        ));
    }

    let keep_temp_cache = std::env::var("IFC2LBD_OCC_CACHE_PERSIST")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("ifc");
    let safe_stem: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let path = std::env::temp_dir()
        .join("ifc2lbd-neo-occ-cache")
        .join(format!("{safe_stem}_{pid}_{now}"));
    std::fs::create_dir_all(&path).with_context(|| {
        format!(
            "failed to create temporary OCC cache dir {}",
            path.display()
        )
    })?;
    tracing::info!(
        "topology-full OCC cache dir: {}{}",
        path.display(),
        if keep_temp_cache {
            " (persist=true)"
        } else {
            " (ephemeral)"
        }
    );
    Ok((
        vec![
            "--brep-cache-dir".to_string(),
            path.to_string_lossy().into_owned(),
        ],
        CacheDirGuard {
            path,
            cleanup_on_drop: !keep_temp_cache,
        },
    ))
}

pub(crate) fn resolve_geometry_kernel_bin() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("IFC2LBD_GEOMETRY_KERNEL_BIN") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Ok(p);
        }
    }

    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("lbd-geometry-kernel"));
        }
    }
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    candidates.push(workspace_root.join("target/release/lbd-geometry-kernel"));
    candidates.push(workspace_root.join("target/debug/lbd-geometry-kernel"));

    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Ok(found);
    }

    tracing::info!("building lbd-geometry-kernel once (auto-discovery path)");
    let mut cargo_build = Command::new("cargo");
    cargo_build
        .arg("build")
        .arg("-p")
        .arg("lbd-geometry-kernel")
        .arg("--bin")
        .arg("lbd-geometry-kernel")
        .current_dir(&workspace_root);
    configure_pyo3_python_env(&mut cargo_build);
    let status = cargo_build
        .status()
        .context("failed to start cargo build for lbd-geometry-kernel")?;
    if !status.success() {
        anyhow::bail!(
            "failed to build lbd-geometry-kernel automatically (status: {})",
            status
        );
    }

    let built = workspace_root.join("target/debug/lbd-geometry-kernel");
    if built.is_file() {
        Ok(built)
    } else {
        anyhow::bail!(
            "lbd-geometry-kernel build finished but binary was not found at {}",
            built.display()
        )
    }
}

pub(crate) fn configure_pyo3_python_env(cmd: &mut Command) {
    if std::env::var_os("PYO3_PYTHON").is_some() {
        return;
    }
    if let Some(python) = detect_python3_executable() {
        tracing::info!("using detected python for pyo3: {}", python.display());
        cmd.env("PYO3_PYTHON", python);
    }
}

pub(crate) fn detect_python3_executable() -> Option<PathBuf> {
    let output = Command::new("python3")
        .arg("-c")
        .arg("import sys; print(sys.executable)")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }
    let resolved = PathBuf::from(path);
    if resolved.is_file() {
        Some(resolved)
    } else {
        None
    }
}
