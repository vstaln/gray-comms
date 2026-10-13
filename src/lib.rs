//! gray-comms: a cross-agent message mesh.
//!
//! A tiny TCP daemon holds rooms, presence, and durable message history;
//! agents (gray sessions, other harnesses, humans) speak newline-delimited
//! JSON — join/leave/say/dm/who/peek — with real-time push events for live
//! listeners. The gray sidecar exposes comms_* tools and a prompt/context
//! hook that notes newly arrived DMs.
pub mod cli;
pub mod client;
pub mod config;
pub mod daemon;
pub mod doctor;
pub mod proto;
pub mod routing;
pub mod service;
pub mod setup;
pub mod sidecar;
pub mod store;
