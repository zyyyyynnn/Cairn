// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Which indexer the window talks to.
//!
//! The installed LocalSystem service is the preferred source; when it is unavailable the app
//! starts a user-privilege indexer of its own on a separate pipe. Queries, status polling and
//! configuration all go through this one switch, so a session never mixes the two.

use cairn_client::client::{CairnClient, ClientError};
use cairn_protocol::message::{PIPE_NAME, USER_PIPE_NAME};
use std::sync::atomic::{AtomicBool, Ordering};

static LOCAL_INDEXER: AtomicBool = AtomicBool::new(false);

/// Routes every later IPC call to the user-mode indexer.
pub fn use_local_indexer() {
    LOCAL_INDEXER.store(true, Ordering::SeqCst);
}

/// Routes every later IPC call to the installed service.
pub fn use_service() {
    LOCAL_INDEXER.store(false, Ordering::SeqCst);
}

pub fn is_local() -> bool {
    LOCAL_INDEXER.load(Ordering::SeqCst)
}

pub fn pipe_name() -> &'static str {
    if is_local() {
        USER_PIPE_NAME
    } else {
        PIPE_NAME
    }
}

/// Connects to whichever indexer is currently selected.
pub async fn connect() -> Result<CairnClient, ClientError> {
    CairnClient::connect_to(pipe_name()).await
}

/// Polls the user pipe while a freshly started local indexer creates its first instance.
pub async fn connect_user() -> Result<CairnClient, ClientError> {
    CairnClient::connect_to(USER_PIPE_NAME).await
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;

    #[test]
    fn the_active_pipe_follows_the_selected_indexer() {
        use_service();
        assert_eq!(pipe_name(), PIPE_NAME);
        use_local_indexer();
        assert_eq!(pipe_name(), USER_PIPE_NAME);
        use_service();
        assert!(
            !is_local(),
            "the service must be the default after leaving local mode"
        );
    }
}
