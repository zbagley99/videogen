//! Pose suggestions from an LLM, for videogen.
//!
//! Talks to any OpenAI-compatible server (llama.cpp, vLLM, Ollama…). Answers are
//! asked for as JSON matching a schema, which servers that support it enforce
//! while generating. Everything that comes back is still parsed leniently and
//! checked against the script and the pose library; problems are repaired where
//! possible (with a follow-up request if the JSON is broken) and reported.

pub mod client;
pub mod json;
pub mod suggest;

pub use client::{Client, Structure};
pub use suggest::{Mode, Note, Suggestion, SuggestOptions, suggest};
