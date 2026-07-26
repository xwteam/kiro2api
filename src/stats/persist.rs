//! 通用 JSON 文件持久化助手:原子写(tmp+rename)+ 约 5s 异步批量刷盘的脏标记驱动。
//!
//! 设计:热路径(record)只改内存 + 置脏标记 + 通过 MPSC 捅一下后台任务,绝不做文件 I/O;
//! 后台任务用一个 5s 周期定时器,每拍检查脏标记,脏则取快照 → `spawn_blocking` 落盘。
//! 通道关闭(优雅退出)时做最后一次刷盘。真正的落盘用 tmp+rename 原子替换。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::Serialize;
use tokio::sync::mpsc;

/// 默认刷盘周期(秒)。
pub const FLUSH_INTERVAL_SECS: u64 = 5;

/// 原子写 JSON:序列化到唯一临时文件 → fsync → rename 覆盖 `path`。同步阻塞,须在
/// `spawn_blocking` 内调用。
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let data = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_bytes_atomic(path, &data)
}

/// 读 JSON;文件不存在返回 `None`(交由调用侧回落默认/空)。
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> std::io::Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let v = serde_json::from_str(&s)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(Some(v))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 每次写盘的临时文件后缀计数器,保证并发写各用独立 tmp 文件(不共享、不互相截断)。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成本次写盘专用的唯一临时路径:`{path}.tmp.{pid}.{counter}`。
/// pid 隔离多进程、进程内单调计数器隔离并发写,避免 fixed `{path}.tmp`
/// 的交错写/rename 竞态损坏落盘文件。
fn tmp_path(path: &Path) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(".tmp.{}.{n}", std::process::id()));
    PathBuf::from(s)
}

/// 陈旧 tmp 判定阈值:比这更久没被动过的 `{path}.tmp.*` 视为进程崩溃遗留的孤儿,可清。
/// 取 5 分钟,远大于一次正常刷盘(写+fsync+rename,亚秒级),故绝不会误删并发写者正在写的 tmp。
const STALE_TMP_AGE_SECS: u64 = 300;

/// #10:清理同目录下遗留的孤儿 `{path}.tmp.*` 文件(进程在 tmp 写完与 rename 之间崩溃留下的)。
/// 与 `kiro::credential.rs::sweep_stale_tmp` 同策略,覆盖 stats/api-keys/balance 落盘路径。
///
/// best-effort:任何 IO 错误都忽略(不使刷盘失败)。只删**足够旧**(mtime 早于 `STALE_TMP_AGE_SECS`)
/// 的 tmp,从而不会碰到另一并发写者此刻正在写的新 tmp(其 mtime 是当下)。前缀严格匹配
/// `{basename}.tmp.` 以免误伤同目录其它文件。
fn sweep_stale_tmp(path: &Path) {
    let (dir, file_name) = match (path.parent(), path.file_name()) {
        (Some(d), Some(f)) => (d, f.to_string_lossy().into_owned()),
        _ => return,
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{file_name}.tmp.");
    let now = std::time::SystemTime::now();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) {
            continue;
        }
        // 仅清足够旧的:mtime 距今 >= 阈值。取不到 mtime 则保守跳过(不删)。
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mtime| now.duration_since(mtime).ok())
            .map(|age| age.as_secs() >= STALE_TMP_AGE_SECS)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 脏标记 + 刷盘触发器。`mark_dirty()` 在热路径调用(无 I/O),后台任务消费。
#[derive(Clone)]
pub struct DirtyFlag {
    dirty: Arc<AtomicBool>,
    tx: mpsc::Sender<()>,
}

impl DirtyFlag {
    /// 置脏并捅一下后台任务(通道满/关时忽略——定时器仍会兜底)。
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
        let _ = self.tx.try_send(());
    }
}

/// 后台刷盘循环所需的一端。`spawn_flush_loop` 消费它。
pub struct FlushHandle {
    dirty: Arc<AtomicBool>,
    rx: mpsc::Receiver<()>,
}

/// 建一对 (DirtyFlag, FlushHandle)。
pub fn dirty_channel() -> (DirtyFlag, FlushHandle) {
    let dirty = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel(1);
    (
        DirtyFlag {
            dirty: dirty.clone(),
            tx,
        },
        FlushHandle { dirty, rx },
    )
}

/// 启动后台刷盘任务:每 `interval_secs` 拍检查脏标记;脏则调用 `snapshot()` 取内存
/// 快照,`spawn_blocking` 写盘。`rx` 关闭(所有 DirtyFlag 被 drop)时做最后一次刷盘并退出。
///
/// `snapshot` 返回已序列化好的 `Vec<u8>` —— 由调用方在持锁下拿快照并序列化,避免把锁
/// 类型泄漏到本模块。需要"本轮不写盘"的语义时用 [`spawn_flush_loop_opt`]。
pub fn spawn_flush_loop<F>(path: PathBuf, handle: FlushHandle, interval_secs: u64, snapshot: F)
where
    F: Fn() -> Vec<u8> + Send + Sync + 'static,
{
    spawn_flush_loop_opt(path, handle, interval_secs, move || Some(snapshot()));
}

/// 同 [`spawn_flush_loop`],但快照闭包返回 `Option<Vec<u8>>`:`None` = **本轮跳过写盘**,
/// 磁盘保留上一次成功落盘的内容。
///
/// 供两类场景用:快照源已析构(后台任务只持 `Weak`,升级失败),或序列化失败——
/// 两种情况下都绝不能把空内容原子覆写上去。
pub fn spawn_flush_loop_opt<F>(path: PathBuf, handle: FlushHandle, interval_secs: u64, snapshot: F)
where
    F: Fn() -> Option<Vec<u8>> + Send + Sync + 'static,
{
    let FlushHandle { dirty, mut rx } = handle;
    let snapshot = Arc::new(snapshot);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    flush_if_dirty(&path, &dirty, &snapshot).await;
                }
                msg = rx.recv() => {
                    if msg.is_none() {
                        // 通道关闭:最后一次刷盘后退出。
                        flush_if_dirty(&path, &dirty, &snapshot).await;
                        break;
                    }
                    // 收到脏通知:不立刻刷,等下一拍统一批量落盘(去抖)。
                }
            }
        }
    });
}

async fn flush_if_dirty<F>(path: &Path, dirty: &AtomicBool, snapshot: &Arc<F>)
where
    F: Fn() -> Option<Vec<u8>> + Send + Sync + 'static,
{
    // 抢占式清脏:先置 false,期间的新写会再次置脏、下拍再落。
    // 快照生成(可能持阻塞读锁)与落盘都在 spawn_blocking 内完成,避免在 async
    // 上下文里调用 blocking_read 触发 panic。
    if dirty.swap(false, Ordering::AcqRel) {
        let path = path.to_path_buf();
        let snapshot = snapshot.clone();
        let res = tokio::task::spawn_blocking(move || match snapshot() {
            Some(bytes) => write_bytes_atomic(&path, &bytes),
            None => Ok(()),
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                dirty.store(true, Ordering::Release); // 落盘失败,保留脏标记下拍重试
                tracing::warn!(error = %e, "stats 刷盘失败");
            }
            Err(e) => {
                dirty.store(true, Ordering::Release);
                tracing::warn!(error = %e, "stats 刷盘任务 join 失败");
            }
        }
    }
}

/// rename 之后 fsync 父目录:让"文件名指向新 inode"这条目录项本身落盘。只 fsync 数据
/// 文件不够——目录项未落盘时,崩溃后该名字可能仍指向 rename 前的旧 inode,本次刷盘整体回退。
///
/// best-effort:打开/同步目录失败不使写盘失败(目录已 rename 成功,数据在页缓存中仍可见)。
#[cfg(unix)]
fn fsync_parent_dir(path: &Path) {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// 非 unix 平台无"目录 fd fsync"语义,空实现。
#[cfg(not(unix))]
fn fsync_parent_dir(_path: &Path) {}

/// 原子写已序列化好的字节:写唯一临时文件 → fsync → rename 覆盖 → fsync 父目录。
///
/// 持久性:rename 前对 tmp 文件 `sync_all`(fsync)保证内容已落盘;rename 后再 fsync
/// 父目录(unix)保证新目录项本身也落盘。二者齐备,进程/系统崩溃后要么完整旧文件、
/// 要么完整新文件,不会半写、也不会整体回退到上一版。tmp 路径按 pid+计数器
/// 唯一化,并发写彼此隔离。写失败/中途出错清理残留 tmp。
///
/// #10:每次写盘顺手清理同目录下遗留的**孤儿** `{path}.tmp.*`(此前进程在 tmp 写完与
/// rename 之间崩溃留下的),仅清足够旧的,不碰并发写者正在写的新 tmp。
pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_path(path);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let res = (|| -> std::io::Result<()> {
        let f = std::fs::File::create(&tmp)?;
        {
            let mut w = std::io::BufWriter::new(&f);
            w.write_all(bytes)?;
            w.flush()?;
        }
        f.sync_all()?; // fsync:数据落盘后再 rename
        std::fs::rename(&tmp, path)?;
        fsync_parent_dir(path); // 目录项落盘,rename 不会在崩溃后回退
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    // best-effort 清理孤儿 tmp(不影响本次写盘结果)。
    sweep_stale_tmp(path);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_then_read_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kiro2api_persist_test_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // 不存在 → None
        let got: Option<Vec<u32>> = read_json(&path).unwrap();
        assert!(got.is_none());
        // 写 → 读回
        atomic_write_json(&path, &vec![1u32, 2, 3]).unwrap();
        let got: Option<Vec<u32>> = read_json(&path).unwrap();
        assert_eq!(got, Some(vec![1, 2, 3]));
        // tmp 已被 rename 掉,不残留
        assert!(!tmp_path(&path).exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_sweeps_stale_orphan_tmp_but_keeps_fresh() {
        use std::time::SystemTime;
        let dir = std::env::temp_dir().join(format!(
            "kiro2api_persist_sweep_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stats.json");
        let base = path.file_name().unwrap().to_string_lossy().into_owned();

        // 造一个"陈旧"孤儿 tmp:用 `touch -d` 把 mtime 回拨到远超阈值(无第三方 crate 依赖)。
        let stale_tmp = dir.join(format!("{base}.tmp.99999.0"));
        std::fs::write(&stale_tmp, b"orphan").unwrap();
        let touched = std::process::Command::new("touch")
            .arg("-d")
            .arg("2000-01-01 00:00:00")
            .arg(&stale_tmp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        // 造一个"新近"孤儿 tmp:mtime 为当下,必须被保留。
        let fresh_tmp = dir.join(format!("{base}.tmp.99999.1"));
        std::fs::write(&fresh_tmp, b"in-flight").unwrap();

        // 同目录无关文件必须保留。
        let unrelated = dir.join("other.json");
        std::fs::write(&unrelated, b"keep").unwrap();

        // 一次正常写盘会触发 sweep。
        write_bytes_atomic(&path, b"{}").unwrap();

        // 陈旧 tmp 被清(仅当 mtime 成功回拨,即 touch 可用时才断言,避免无 touch 环境误报)。
        if touched {
            assert!(!stale_tmp.exists(), "陈旧孤儿 tmp 应被写盘清理");
        }
        assert!(fresh_tmp.exists(), "新近(并发写者的)tmp 必须保留");
        assert!(unrelated.exists(), "同目录无关文件必须保留");
        assert!(path.exists(), "目标文件应写入成功");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_into_nested_dir_and_dir_fsync_tolerates_bare_name() {
        use std::time::SystemTime;
        let dir = std::env::temp_dir().join(format!(
            "kiro2api_persist_dirsync_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // 目标目录尚不存在:写盘应自行创建、rename 后 fsync 父目录,不因此失败。
        let path = dir.join("nested").join("stats.json");
        write_bytes_atomic(&path, b"[1,2,3]").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"[1,2,3]".to_vec());
        // 无父目录的裸文件名:fsync 回落到 "." ,不 panic、不报错。
        fsync_parent_dir(Path::new("stats.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn flush_loop_skips_write_when_snapshot_is_none() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kiro2api_flush_skip_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // 先落一份已知内容,模拟"上一次成功刷盘"。
        atomic_write_json(&path, &vec![7u32]).unwrap();

        let (flag, handle) = dirty_channel();
        // 快照源已消失 → None:即便置脏也不得把空内容覆写上去。
        spawn_flush_loop_opt(path.clone(), handle, 1, || None);
        flag.mark_dirty();
        tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
        let got: Option<Vec<u32>> = read_json(&path).unwrap();
        assert_eq!(got, Some(vec![7]), "快照返回 None 时磁盘内容必须原样保留");

        // 通道关闭走的最后一次刷盘同样不得覆写。
        drop(flag);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let got2: Option<Vec<u32>> = read_json(&path).unwrap();
        assert_eq!(got2, Some(vec![7]));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn flush_loop_persists_on_dirty_and_on_close() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("kiro2api_flush_test_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let (flag, handle) = dirty_channel();
        let payload = Arc::new(std::sync::Mutex::new(vec![10u32, 20]));
        let p2 = payload.clone();
        // 1s 周期便于测试
        spawn_flush_loop(path.clone(), handle, 1, move || {
            serde_json::to_vec_pretty(&*p2.lock().unwrap()).unwrap()
        });

        flag.mark_dirty();
        // 等一拍多一点让定时器落盘
        tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
        let got: Option<Vec<u32>> = read_json(&path).unwrap();
        assert_eq!(got, Some(vec![10, 20]));

        // 改内存 + 置脏 + drop flag(关通道)→ 应触发最后一次刷盘
        *payload.lock().unwrap() = vec![99];
        flag.mark_dirty();
        drop(flag);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let got: Option<Vec<u32>> = read_json(&path).unwrap();
        assert_eq!(got, Some(vec![99]));

        let _ = std::fs::remove_file(&path);
    }
}
