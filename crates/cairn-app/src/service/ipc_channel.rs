// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The service pipe every IPC call goes through.

use cairn_client::client::{CairnClient, ClientError};
use cairn_protocol::message::PIPE_NAME;

/// Connects to the installed service.
pub async fn connect() -> Result<CairnClient, ClientError> {
    CairnClient::connect_to(PIPE_NAME).await
}
