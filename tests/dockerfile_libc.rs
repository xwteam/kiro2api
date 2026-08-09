//! 构建镜像与运行镜像的 glibc 世代必须一致。
//!
//! 二进制在 builder 里链接、按 builder 的 libc 头定符号版本,却在运行镜像的 glibc 上跑。
//! builder 比运行镜像新,就会缺符号 —— 而且**编译、镜像构建、推送全都成功**,只在容器启动
//! 那一刻炸,CI 一路绿灯。v0.9.1 就是这么把生产打挂的:裸 `rust:1-slim` 随上游滚到了
//! trixie(glibc 2.41),运行镜像仍是 bookworm(2.36),vendored OpenSSL 又用到 glibc 2.38
//! 才加入的 strlcpy/strlcat,于是启动即 `version GLIBC_2.38 not found`。
//!
//! 故这里把"两边同代"钉成断言:换任一边都必须同步换另一边,否则测试先红,而不是生产先红。

/// builder 与运行镜像必须锁在同一个 Debian 世代,且 builder 不得使用会随上游漂移的浮动标签。
#[test]
fn the_builder_and_runtime_glibc_generations_match() {
    let dockerfile = include_str!("../Dockerfile");

    let builder = dockerfile
        .lines()
        .find(|l| l.starts_with("FROM") && l.contains("rust:"))
        .expect("Dockerfile 里应有 rust builder 阶段");
    let runtime = dockerfile
        .lines()
        .find(|l| l.starts_with("FROM") && l.contains("debian:"))
        .expect("Dockerfile 里应有 debian 运行阶段");

    // 已知的 Debian 世代,由旧到新。两边取到的必须是同一个。
    const SUITES: [&str; 4] = ["bullseye", "bookworm", "trixie", "forky"];
    let suite_of = |line: &str| SUITES.iter().find(|s| line.contains(*s)).copied();

    let b = suite_of(builder).unwrap_or_else(|| {
        panic!("builder 未钉 Debian 世代(浮动标签会随上游滚到更新的 glibc):{builder}")
    });
    let r = suite_of(runtime)
        .unwrap_or_else(|| panic!("运行镜像未钉 Debian 世代:{runtime}"));

    assert_eq!(
        b, r,
        "builder({b})与运行镜像({r})的 Debian 世代不一致 —— 产出的二进制会在启动时缺 glibc 符号。\n  builder: {builder}\n  runtime: {runtime}"
    );
}
