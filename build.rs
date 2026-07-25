use std::process::Command;

/// 构建脚本:在编译期捕获 rustc 版本字符串,注入编译期环境变量
/// `KIRO_RUST_VERSION`,供 `env!()` 在 server-info 端点读取展示(对齐
/// gemini2api 展示 'Python 版本' 的做法,本项目为 Rust 故展示 'Rust 版本')。
///
/// best-effort:优先用 Cargo 传入的 RUSTC 路径调用 `rustc --version`;
/// 任一环节失败(取不到路径/命令失败/输出非 UTF-8/为空)一律回落 "unknown",
/// 绝不让构建失败。因此下游 `env!("KIRO_RUST_VERSION")` 始终有值。
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let v = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=KIRO_RUST_VERSION={v}");
}
