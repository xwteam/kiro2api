use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// 实时日志广播通道容量:单连接落后超过此值即丢弃最旧的一批(见 SSE 处理的 Lagged 处理)。
/// 与历史环形缓冲区容量(可配)相互独立——前者服务活跃订阅者的即时扇出,后者服务新连接的历史回放。
const BROADCAST_CAPACITY: usize = 512;

/// 单条结构化日志记录。字段名(camelCase)与前端 `LogEntry` 契约对齐:
/// timestamp / level / target / message。序列化为 JSON 供 SSE 与快照端点直出。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogEntry {
    /// ISO 8601 UTC、毫秒精度、带 `Z` 后缀,例:`2026-07-20T14:30:45.123Z`。
    pub timestamp: String,
    /// 级别文本:TRACE / DEBUG / INFO / WARN / ERROR。
    pub level: String,
    /// 事件来源目标(通常是 `crate::module` 路径)。
    pub target: String,
    /// 展平后的消息文本(含 `message` 字段主体 + 其余字段的 `k=v` 拼接)。
    pub message: String,
}

/// 日志捕获器:同时驱动两条数据面——
/// 1) 有界历史环形缓冲(`ring`,FIFO 淘汰),供新连接一次性回放最近若干条;
/// 2) 广播发送端(`sender`),向所有活跃 SSE 订阅者实时扇出每条新记录。
///
/// tracing 的 `LogCaptureLayer`(由 [`layer`](LogCapture::layer) 产出)在每次事件到达时
/// 把同一条 [`LogEntry`] 既压入环形缓冲、又通过广播发送——两者共享 `Arc`,故 admin 状态持有的
/// `LogCapture` 与订阅层看到的是同一份数据。
pub struct LogCapture {
    ring: Arc<Mutex<VecDeque<LogEntry>>>,
    sender: broadcast::Sender<LogEntry>,
    capacity: usize,
}

impl LogCapture {
    /// 以给定历史容量新建捕获器。`capacity` 为环形缓冲最大条数(0 视为 1,避免空缓冲边界)。
    /// 广播通道容量固定为 [`BROADCAST_CAPACITY`],与历史容量解耦。
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            ring: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            sender,
            capacity,
        }
    }

    /// 产出可注册进 `tracing_subscriber` registry 的捕获层。可多次调用(各自持有共享句柄的克隆)。
    pub fn layer(&self) -> LogCaptureLayer {
        LogCaptureLayer {
            ring: self.ring.clone(),
            sender: self.sender.clone(),
            capacity: self.capacity,
        }
    }

    /// 当前历史环形缓冲的完整拷贝(最旧→最新)。供快照/下载端点与新 SSE 连接的历史回放使用。
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.ring.lock().iter().cloned().collect()
    }

    /// 订阅实时广播,返回接收端。新 SSE 连接应先 `subscribe()` 再 `snapshot()`,
    /// 以确保订阅建立与历史读取之间新产生的记录不会两头漏接(见 SSE 处理器注释)。
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.sender.subscribe()
    }

    /// 直接注入一条记录到环形缓冲 + 广播(不经 tracing 层)。仅测试与内部使用。
    #[cfg(test)]
    pub(crate) fn push_entry(&self, entry: LogEntry) {
        push_to_ring(&self.ring, self.capacity, entry.clone());
        let _ = self.sender.send(entry);
    }
}

/// 把一条记录写入有界环形缓冲:满则先淘汰最旧一条,再压入新条(FIFO)。
fn push_to_ring(ring: &Arc<Mutex<VecDeque<LogEntry>>>, capacity: usize, entry: LogEntry) {
    let mut buf = ring.lock();
    while buf.len() >= capacity {
        buf.pop_front();
    }
    buf.push_back(entry);
}

/// tracing 捕获层:实现 [`Layer::on_event`],把每条事件规整为 [`LogEntry`] 后同时写入
/// 历史环形缓冲与广播通道。持有的三个字段均为对 [`LogCapture`] 内部状态的共享克隆。
pub struct LogCaptureLayer {
    ring: Arc<Mutex<VecDeque<LogEntry>>>,
    sender: broadcast::Sender<LogEntry>,
    capacity: usize,
}

impl<S: Subscriber> Layer<S> for LogCaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            Level::TRACE => "TRACE",
            Level::DEBUG => "DEBUG",
            Level::INFO => "INFO",
            Level::WARN => "WARN",
            Level::ERROR => "ERROR",
        };

        let mut message = String::new();
        event.record(&mut MessageVisitor(&mut message));

        let entry = LogEntry {
            timestamp: chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
            level: level.to_string(),
            target: metadata.target().to_string(),
            message,
        };

        push_to_ring(&self.ring, self.capacity, entry.clone());
        // 无活跃订阅者时 send 返回 Err,忽略即可——历史仍已入环形缓冲。
        let _ = self.sender.send(entry);
    }
}

/// 从 tracing 事件字段提取文本:`message` 字段作为主体,其余字段以 `k=v` 追加(空格分隔)。
struct MessageVisitor<'a>(&'a mut String);

impl<'a> MessageVisitor<'a> {
    fn append_kv(&mut self, key: &str, value: &str) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(key);
        self.0.push('=');
        self.0.push_str(value);
    }
}

impl<'a> Visit for MessageVisitor<'a> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            self.append_kv(field.name(), value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            // Debug 会给字符串加引号,主体消息去掉外层引号更贴近原文。
            let trimmed = rendered
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(&rendered);
            self.0.push_str(trimmed);
        } else {
            self.append_kv(field.name(), &rendered);
        }
    }
}

/// 初始化全局 tracing 订阅者。`log_capture` 为 `Some` 时把捕获层挂进 registry,
/// 使所有事件在正常 stdout 输出之余,同步进入历史缓冲 + 实时广播;`None` 则仅 stdout。
///
/// 保持既有行为:fmt 层 + 环境过滤(默认 `info`)始终存在,捕获层是可选叠加,不改变原有日志。
pub fn init_tracing(log_capture: Option<Arc<LogCapture>>) {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let capture_layer = log_capture.map(|c| c.layer());
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(capture_layer)
        .with(filter)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(msg: &str) -> LogEntry {
        LogEntry {
            timestamp: "2026-07-20T14:00:00.000Z".into(),
            level: "INFO".into(),
            target: "test".into(),
            message: msg.into(),
        }
    }

    #[test]
    fn ring_caps_and_orders_history_fifo() {
        let cap = LogCapture::new(2);
        cap.push_entry(entry("a"));
        cap.push_entry(entry("b"));
        cap.push_entry(entry("c")); // 挤掉最旧的 "a"

        let snap = cap.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].message, "b");
        assert_eq!(snap[1].message, "c");
    }

    #[test]
    fn zero_capacity_is_clamped_to_one() {
        let cap = LogCapture::new(0);
        cap.push_entry(entry("only"));
        let snap = cap.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].message, "only");
    }

    #[tokio::test]
    async fn broadcast_fans_out_to_multiple_subscribers() {
        let cap = LogCapture::new(8);
        let mut rx1 = cap.subscribe();
        let mut rx2 = cap.subscribe();

        cap.push_entry(entry("live-1"));
        cap.push_entry(entry("live-2"));

        assert_eq!(rx1.recv().await.unwrap().message, "live-1");
        assert_eq!(rx1.recv().await.unwrap().message, "live-2");
        assert_eq!(rx2.recv().await.unwrap().message, "live-1");
        assert_eq!(rx2.recv().await.unwrap().message, "live-2");
    }

    #[tokio::test]
    async fn subscriber_only_sees_entries_after_subscription() {
        let cap = LogCapture::new(8);
        // 订阅前的记录只进历史,不进这个订阅者的实时流。
        cap.push_entry(entry("before"));
        let mut rx = cap.subscribe();
        cap.push_entry(entry("after"));

        // 历史含两条(before + after)。
        let snap = cap.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].message, "before");
        assert_eq!(snap[1].message, "after");

        // 实时流只有订阅后的 "after"。
        assert_eq!(rx.recv().await.unwrap().message, "after");
    }

    #[test]
    fn log_entry_serializes_camelcase_shape() {
        let e = entry("hello");
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["timestamp"], "2026-07-20T14:00:00.000Z");
        assert_eq!(v["level"], "INFO");
        assert_eq!(v["target"], "test");
        assert_eq!(v["message"], "hello");
        // 仅这四个字段,无多余键。
        assert_eq!(v.as_object().unwrap().len(), 4);
    }

    #[test]
    fn snapshot_shape_is_json_array_of_entries() {
        let cap = LogCapture::new(4);
        cap.push_entry(entry("x"));
        cap.push_entry(entry("y"));
        let snap = cap.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["message"], "x");
        assert_eq!(parsed[1]["message"], "y");
    }
}
