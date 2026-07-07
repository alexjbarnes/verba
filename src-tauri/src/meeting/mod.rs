//! Meeting mode (desktop only): live dual-stream transcription with local LLM
//! summaries. See MODEL_PACKAGES.md and the meeting-mode plan.
//!
//! Grows over the implementation phases. Phase 1 lands only the loopback
//! resolver; the session coordinator, store, speakers, and summarizer follow.

pub mod loopback;
pub mod summarize;
