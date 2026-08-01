//! 测试用临时目录的统一出口(仅测试期编译)。
//!
//! 起因:各模块的测试各自用 `std::env::temp_dir().join(format!("kiro2api_xxx_{pid}_{nanos}"))`
//! 手搓路径,建完从不删。跑一轮完整测试会在 /tmp 顶层留下数千个目录,且**永不回收**——
//! 2026-08-01 排查磁盘问题时,/tmp 里堆了 9582 个这样的残留(仅 4 天积累)。
//! systemd-tmpfiles 对 /tmp 的老化策略是 30 天,远慢于产生速度。
//!
//! 做法:所有测试临时产物收进**单一的 per-process 根目录** `/tmp/kiro2api-tests/<pid>/`,
//! 并在每个测试进程启动时(`Once`,只跑一次)回收掉那些**属于已退出进程**的旧根目录。
//! 于是 /tmp 顶层最多只有 `kiro2api-tests` 一个条目,残留上限是"一轮测试",
//! 而且下一轮测试会自动把它收走。
//!
//! 这样设计而不是改成返回 RAII guard,是为了不动那 100+ 个调用点的形态
//! (`empty_stats("tag")` 这类内联调用改成元组会把测试写法搅乱)。

use std::path::{Path, PathBuf};
use std::sync::Once;

/// 所有测试临时产物的公共父目录。
fn tests_root() -> PathBuf {
    std::env::temp_dir().join("kiro2api-tests")
}

/// 本进程专属的根目录。
fn process_root() -> PathBuf {
    tests_root().join(std::process::id().to_string())
}

/// 判断某个 pid 是否还活着(Linux:看 /proc/<pid> 在不在)。
fn pid_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// 回收上几轮测试留下的根目录。只删**进程已退出**的那些,
/// 因此并行跑多个测试二进制时互不误伤。
fn sweep_dead_roots() {
    let root = tests_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid == me || pid_alive(pid) {
            continue;
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// 进程内只扫一次。
fn ensure_swept() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = std::fs::create_dir_all(tests_root());
        sweep_dead_roots();
    });
}

/// 返回一个本进程内唯一的临时**目录**路径(已创建好)。
///
/// `tag` 只用于让人看得懂是哪个测试留下的,不参与唯一性保证。
pub fn dir(tag: &str) -> PathBuf {
    ensure_swept();
    let path = process_root().join(format!("{tag}-{}", next_seq()));
    let _ = std::fs::create_dir_all(&path);
    path
}

/// 返回一个本进程内唯一的临时**文件**路径(父目录已建好,文件本身不创建)。
///
/// 之所以每个文件也单独套一层目录,是因为不少测试会在同一路径旁边写
/// `<name>.next-id`、`<name>.tmp` 之类的伴生文件——放进各自的目录里才不会互相踩。
pub fn file(tag: &str, filename: &str) -> PathBuf {
    dir(tag).join(filename)
}

/// 返回**同一 tag 恒定同一个**的临时目录路径,并保证调用后它是空的。
///
/// 给那些"同名残留先清掉"语义的老 helper 用(原来是 `temp_dir()/kiro2api_x_<pid>`,
/// 不含纳秒,所以同 tag 反复调用拿到同一个路径)。
pub fn stable_dir(tag: &str) -> PathBuf {
    ensure_swept();
    let path = process_root().join(tag);
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::create_dir_all(&path);
    path
}

/// 返回**同一 tag 恒定同一个**的临时文件路径(父目录已建好,内容不动)。
///
/// 给那些"先写后读同一路径"的老 helper 用——这类路径原本也不含纳秒,
/// 一个用例内多次取要拿到同一个文件,不能每次给新的。
pub fn stable_file(tag: &str, filename: &str) -> PathBuf {
    ensure_swept();
    let parent = process_root().join(tag);
    let _ = std::fs::create_dir_all(&parent);
    parent.join(filename)
}

/// 进程内自增序号,保证同一 tag 多次调用不撞。
fn next_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_are_unique_and_under_process_root() {
        let a = dir("same_tag");
        let b = dir("same_tag");
        assert_ne!(a, b, "同一 tag 两次调用必须拿到不同目录");
        assert!(a.is_dir() && b.is_dir(), "目录应当已经创建好");
        assert!(a.starts_with(process_root()));
        assert!(b.starts_with(process_root()));
    }

    #[test]
    fn file_paths_get_their_own_parent_dir() {
        let p = file("creds", "credentials.json");
        assert_eq!(p.file_name().unwrap(), "credentials.json");
        assert!(
            p.parent().unwrap().is_dir(),
            "父目录必须已创建,调用方才能直接写文件"
        );
        assert!(!p.exists(), "文件本身不应被预先创建");
    }

    #[test]
    fn stable_paths_stay_put_across_calls() {
        // 同 tag 的文件路径必须恒定,否则"先写后读"的老用例会读空
        let a = stable_file("stable_tag", "store.json");
        std::fs::write(&a, b"hello").unwrap();
        let b = stable_file("stable_tag", "store.json");
        assert_eq!(a, b);
        assert_eq!(std::fs::read(&b).unwrap(), b"hello");

        // 而稳定目录必须每次清空(对应老 helper 的"同名残留先清掉")
        let d1 = stable_dir("stable_dir_tag");
        std::fs::write(d1.join("leftover"), b"x").unwrap();
        let d2 = stable_dir("stable_dir_tag");
        assert_eq!(d1, d2);
        assert!(!d2.join("leftover").exists(), "稳定目录再取时必须已清空");
    }

    #[test]
    fn sweep_removes_only_dead_process_roots() {
        ensure_swept();
        // 造一个不可能存活的 pid 目录(u32::MAX 远超 pid_max)
        let dead = tests_root().join(u32::MAX.to_string());
        std::fs::create_dir_all(&dead).unwrap();
        let mine = dir("keepme");

        sweep_dead_roots();

        assert!(!dead.exists(), "已退出进程的根目录应被回收");
        assert!(mine.is_dir(), "本进程的目录不能被误删");
    }
}
