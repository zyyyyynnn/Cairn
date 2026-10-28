// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Service connection: the client pipe channel, background IPC tasks, and
//! configuration submission to the indexing service.

pub(crate) mod config_submission;
pub(crate) mod elevation;
pub(crate) mod ipc_channel;
pub(crate) mod ipc_tasks;
pub(crate) mod scope_switch;
pub(crate) mod search_scope;
pub(crate) mod service_control;
pub(crate) mod service_issue;
