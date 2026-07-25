//! OpenAI Chat Completions 协议 DTO 与处理器。
pub mod convert;
pub mod handler;
pub mod types;

pub use handler::openai_router;
