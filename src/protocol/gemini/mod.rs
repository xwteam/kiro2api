//! Gemini(Google Generative Language v1beta)协议模块树。
pub mod convert;
pub mod handler;
pub mod types;

pub use handler::gemini_router;
