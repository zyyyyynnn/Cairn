// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `cairn-service` library: Engine lifecycle, persistence, IPC pipe server, and security.

pub mod caller_auth;
pub mod config;
pub mod exclusion;
pub mod filetime;
pub mod persistence;
pub mod pipe_server;
pub mod security;
pub mod service;
pub mod session;
pub mod startup;
pub mod sync_engine;
pub mod user_mode;
