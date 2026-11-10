// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! OS error codes behind this crate's failures. Tantivy stores its `io::Error`s inside error
//! variants that do not implement `Error::source`, so walking the source chain alone can stop
//! before the OS code is reached.

use std::error::Error;
use std::io;
use tantivy::directory::error::{LockError, OpenDirectoryError, OpenReadError, OpenWriteError};
use tantivy::TantivyError;

/// The OS error code of an extraction or indexing failure, searched across the whole source
/// chain and inside Tantivy's variants, which keep the `io::Error` private.
pub fn raw_os_error(error: &(dyn Error + 'static)) -> Option<i32> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(code) = os_code_of(err) {
            return Some(code);
        }
        current = err.source();
    }
    None
}

fn os_code_of(error: &(dyn Error + 'static)) -> Option<i32> {
    if let Some(io_error) = error.downcast_ref::<io::Error>() {
        return io_error.raw_os_error();
    }
    let tantivy_error = error.downcast_ref::<TantivyError>()?;
    match tantivy_error {
        TantivyError::IoError(io_error) => io_error.raw_os_error(),
        TantivyError::LockFailure(lock_error, _) => match lock_error {
            LockError::IoError(io_error) => io_error.raw_os_error(),
            LockError::LockBusy => None,
        },
        TantivyError::OpenDirectoryError(error) => match error {
            OpenDirectoryError::IoError { io_error, .. } => io_error.raw_os_error(),
            OpenDirectoryError::FailedToCreateTempDir(io_error) => io_error.raw_os_error(),
            _ => None,
        },
        TantivyError::OpenReadError(OpenReadError::IoError { io_error, .. }) => {
            io_error.raw_os_error()
        }
        TantivyError::OpenWriteError(OpenWriteError::IoError { io_error, .. }) => {
            io_error.raw_os_error()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::ContentIndexError;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn io_error(code: i32) -> Arc<io::Error> {
        Arc::new(io::Error::from_raw_os_error(code))
    }

    #[test]
    fn tantivy_variants_expose_the_os_code_hidden_from_the_source_chain() {
        let io_wrapped = TantivyError::IoError(io_error(32));
        assert_eq!(raw_os_error(&io_wrapped), Some(32));

        let lock_wrapped = TantivyError::LockFailure(
            LockError::IoError(io_error(5)),
            Some("writer lock".to_string()),
        );
        assert_eq!(raw_os_error(&lock_wrapped), Some(5));

        let directory_wrapped = TantivyError::OpenDirectoryError(OpenDirectoryError::IoError {
            io_error: io_error(5),
            directory_path: PathBuf::from(r"C:\ProgramData\Cairn\content"),
        });
        assert_eq!(raw_os_error(&directory_wrapped), Some(5));

        let write_wrapped = TantivyError::OpenWriteError(OpenWriteError::IoError {
            io_error: io_error(32),
            filepath: PathBuf::from("segment.meta"),
        });
        assert_eq!(raw_os_error(&write_wrapped), Some(32));

        assert_eq!(
            raw_os_error(&TantivyError::LockFailure(LockError::LockBusy, None)),
            None,
            "a busy lock carries no OS code"
        );
    }

    #[test]
    fn the_indexer_error_wrapper_reaches_the_tantivy_code() {
        let error = ContentIndexError::Tantivy(TantivyError::IoError(io_error(32)));
        assert_eq!(
            raw_os_error(&error),
            Some(32),
            "the worker boxes ContentIndexError, so the code must survive that wrapper"
        );
    }

    #[test]
    fn plain_io_errors_keep_their_code() {
        assert_eq!(
            raw_os_error(&io::Error::from_raw_os_error(5)),
            Some(5),
            "batch file opens fail before Tantivy is involved"
        );
        assert_eq!(
            raw_os_error(&io::Error::from(io::ErrorKind::NotFound)),
            None
        );
    }
}
