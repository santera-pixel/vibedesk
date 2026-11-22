//! The channeler: a supervised test input channel.
//!
//! The agent builds things and cannot check them. This module gives it four
//! verbs — look, act, expect, issues — over whatever it just built, and reports
//! what broke in the same breath as the action that broke it.
//!
//! The design is shaped by one measurement: a CDP call costs milliseconds and
//! an agent turn costs seconds, so the tools take sequences rather than single
//! actions. A hundred actions as a hundred tool calls is a hundred turns.
//!
//! See `docs/PLAN-channeler.md`.

pub mod backend;
#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod cdp;
pub mod engine;
pub mod guardrail;
pub mod host;
pub mod issues;
pub mod mcp;
pub mod process;
pub mod sequence;
#[cfg(windows)]
pub mod webview;
#[cfg(windows)]
pub mod window;
