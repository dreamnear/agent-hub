// Tauri 桌面壳构建脚本。
// #5 附加职责：sidecar 构建期刷新——tauri externalBin 在 desktop 构建时把
// binaries/server-<triple> 拷进 target/<profile>/server（dev 形态落位），陈旧的
// 手放产物会把 cargo 刚构建的新 server bin 覆盖成旧版 → 全仓测试 node_cli
// 4 例假失败。此处在每次 desktop 构建前用当前最新的 server 产物刷新 sidecar：
// 仅当 source 较新才拷（mtime 比较，零变化不动文件、不扰构建缓存）；source
// 不存在（fresh clone 只建 desktop）→ 跳过——sidecar 本就 gitignore，先建
// server 再打包的既有流程不变。
use std::{env, path::PathBuf};

fn main() {
    refresh_sidecar();
    tauri_build::build()
}

fn refresh_sidecar() {
    let Ok(triple) = env::var("TARGET") else {
        return; // 无 TARGET（异常环境）→ 不动 sidecar，交给原流程
    };
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    // CARGO_MANIFEST_DIR = <root>/desktop/src-tauri → workspace 根在两级之上
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_dir = env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            manifest
                .parent()
                .and_then(|d| d.parent())
                .map(|root| root.join("target"))
                .expect("manifest 两级父目录恒存在")
        });
    let src = target_dir.join(&profile).join("server");
    let dest_dir = manifest.join("binaries");
    let dest = dest_dir.join(format!("server-{triple}"));
    let Ok(src_meta) = src.metadata() else {
        return; // 无 server 产物（fresh clone 只建 desktop）→ 跳过
    };
    let newer = dest
        .metadata()
        .ok()
        .and_then(|d| d.modified().ok())
        .zip(src_meta.modified().ok())
        .map(|(d, s)| s > d)
        .unwrap_or(true); // dest 缺失 → 直接落
    if !newer {
        return;
    }
    let _ = std::fs::create_dir_all(&dest_dir);
    match std::fs::copy(&src, &dest) {
        Ok(_) => println!(
            "cargo:warning=sidecar refreshed: {} -> {}",
            src.display(),
            dest.display()
        ),
        Err(e) => println!("cargo:warning=sidecar refresh failed ({e}); keeping existing"),
    }
}
