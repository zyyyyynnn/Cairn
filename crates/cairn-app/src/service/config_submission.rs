// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The GUI half of index-configuration changes: read the live configuration over the pipe, edit it,
//! and submit the edited document back to the service.
//!
//! Index configuration is a local-user operation. The service owns `service.toml`, validates the
//! document, persists it with the SYSTEM/Administrators descriptor and applies it without a UAC
//! prompt. The only administrative guard is `admin_only_query`, which the service refuses to let a
//! standard user change.

use cairn_client::client::{CairnClient, ClientError};

/// The index-configuration change this build can submit.
///
/// There is no scope editor yet (next milestone), so the one real setting the window can drive from
/// its own state is the content-index master switch, which is what the search box's "content
/// search" capsule depends on (CNT-01, CNT-06).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexScopeChange {
    /// Write an explicit `content_index_enabled` value. A switch submits the state it is meant to
    /// show, so a retry after an ambiguous failure cannot flip the setting back by accident.
    SetContentIndex { enabled: bool },
}

/// A change could not be prepared out of the configuration the service reported.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the service configuration could not be edited: {reason}")]
pub struct ConfigEditError {
    /// Technical reason, straight from the parser.
    pub reason: String,
}

/// Applies one change to a configuration document, keeping every field the GUI does not know about.
///
/// The service receives the whole document, so dropping a field the GUI does not model would be a
/// silent configuration loss: the document is edited as data, not re-derived from a type.
pub fn apply_change(
    config_toml: &str,
    change: IndexScopeChange,
) -> Result<String, ConfigEditError> {
    let mut document: toml::Table =
        config_toml
            .parse::<toml::Table>()
            .map_err(|err| ConfigEditError {
                reason: err.to_string(),
            })?;

    match change {
        IndexScopeChange::SetContentIndex { enabled } => {
            document.insert(
                "content_index_enabled".to_string(),
                toml::Value::Boolean(enabled),
            );
        }
    }

    toml::to_string_pretty(&document).map_err(|err| ConfigEditError {
        reason: err.to_string(),
    })
}

/// The condition a submission ended in (SPEC SVC-04, SVC-05: state must be
/// visible, never one generic "failed").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionOutcome {
    /// The service validated, persisted and applied the document.
    Applied,
    /// The service refused the document (invalid content, or a policy refusal such as a standard
    /// user changing `admin_only_query`).
    ServiceRefused,
    /// The service could not be reached, or did not answer in this protocol version.
    ServiceUnreachable,
    /// The configuration could not be read or edited before submission.
    PrepareFailed,
}

/// Where a submission stands: `run_content_action` folds the finished state
/// into its own error reporting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmissionState {
    /// The submission reached one of its endings.
    Finished {
        outcome: SubmissionOutcome,
        /// Technical reason shown beside the message: a service answer.
        detail: Option<String>,
    },
}

/// Maps what the service answered onto the state the band shows.
///
/// A policy refusal (`Ok((false, _))` or `Unauthorized`) is kept apart from an unreachable service:
/// the first means the document was rejected, the second means it was never delivered.
pub fn submission_from_service_answer(
    result: Result<(bool, String), ClientError>,
) -> SubmissionState {
    let (outcome, detail) = match result {
        Ok((true, _)) => (SubmissionOutcome::Applied, None),
        Ok((false, message)) => (SubmissionOutcome::ServiceRefused, Some(message)),
        Err(error) if error.is_unauthorized() => {
            (SubmissionOutcome::ServiceRefused, Some(error.to_string()))
        }
        Err(error) => (
            SubmissionOutcome::ServiceUnreachable,
            Some(error.to_string()),
        ),
    };
    SubmissionState::Finished { outcome, detail }
}

/// Runs the whole submission against the running service.
///
/// Sequence: read the live configuration over the pipe, edit it, send the edited document back via
/// `SetConfig`. No elevation is involved; the service persists the document as LocalSystem.
pub async fn submit_index_scope(change: IndexScopeChange) -> SubmissionState {
    let live = match read_live_config().await {
        Ok(live) => live,
        Err(error) => return prepare_failed(error.to_string()),
    };

    let updated = match apply_change(&live, change) {
        Ok(updated) => updated,
        Err(error) => return prepare_failed(error.to_string()),
    };

    submission_from_service_answer(submit_config(&updated).await)
}

/// The configuration the service runs, in the TOML form `GetConfig` reports.
pub type ServiceConfigDocument = String;

fn prepare_failed(detail: String) -> SubmissionState {
    SubmissionState::Finished {
        outcome: SubmissionOutcome::PrepareFailed,
        detail: Some(detail),
    }
}

/// Reads the configuration the running service reports (read class).
async fn read_live_config() -> Result<ServiceConfigDocument, ClientError> {
    let mut client = crate::service::ipc_channel::connect().await?;
    client.hello().await?;
    let envelope = client.get_config().await?;

    match CairnClient::unwrap_service_response(envelope.payload)? {
        cairn_protocol::message::ResponsePayload::ConfigResult { config_toml } => Ok(config_toml),
        _ => Err(ClientError::UnexpectedResponse),
    }
}

/// Sends the edited document to the service, which persists and applies it.
async fn submit_config(document: &str) -> Result<(bool, String), ClientError> {
    let mut client = crate::service::ipc_channel::connect().await?;
    client.hello().await?;
    client.submit_service_config(document).await
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;

    #[test]
    fn service_answers_map_to_the_right_state() {
        assert_eq!(
            submission_from_service_answer(Ok((true, "applied".to_string()))),
            SubmissionState::Finished {
                outcome: SubmissionOutcome::Applied,
                detail: None
            }
        );
        assert_eq!(
            submission_from_service_answer(Ok((false, "invalid document".to_string()))),
            SubmissionState::Finished {
                outcome: SubmissionOutcome::ServiceRefused,
                detail: Some("invalid document".to_string())
            }
        );
        assert!(matches!(
            submission_from_service_answer(Err(ClientError::Unauthorized {
                message: "admin_only_query is administrator-only".to_string()
            })),
            SubmissionState::Finished {
                outcome: SubmissionOutcome::ServiceRefused,
                ..
            }
        ));
        assert!(matches!(
            submission_from_service_answer(Err(ClientError::ConnectionClosed)),
            SubmissionState::Finished {
                outcome: SubmissionOutcome::ServiceUnreachable,
                ..
            }
        ));
    }

    /// The document replaces `service.toml` as a whole, so fields the GUI does not model have to
    /// survive the edit.
    #[test]
    fn toggling_the_content_index_keeps_every_other_field() {
        let live = r#"admin_only_query = false
content_index_enabled = false
content_index_max_bytes = 10485760
excluded_drives = ['F']
excluded_paths = ["$Recycle.Bin", "System Volume Information"]
"#;

        let updated =
            apply_change(live, IndexScopeChange::SetContentIndex { enabled: true }).unwrap();
        let document = updated.parse::<toml::Table>().unwrap();

        assert_eq!(document["content_index_enabled"].as_bool(), Some(true));
        assert_eq!(
            document["excluded_paths"].as_array().unwrap().len(),
            2,
            "exclusion rules must survive the round trip: {updated}"
        );
        assert_eq!(document["excluded_drives"].as_array().unwrap().len(), 1);
        assert_eq!(
            document["content_index_max_bytes"].as_integer(),
            Some(10485760)
        );
        assert_eq!(document["admin_only_query"].as_bool(), Some(false));

        let set_false = apply_change(
            &updated,
            IndexScopeChange::SetContentIndex { enabled: false },
        )
        .unwrap();
        assert_eq!(
            set_false.parse::<toml::Table>().unwrap()["content_index_enabled"].as_bool(),
            Some(false),
            "the switch can turn the setting back off explicitly"
        );
        let set_false_again = apply_change(
            &set_false,
            IndexScopeChange::SetContentIndex { enabled: false },
        )
        .unwrap();
        assert_eq!(
            set_false_again.parse::<toml::Table>().unwrap()["content_index_enabled"].as_bool(),
            Some(false),
            "a retry is idempotent: it writes the requested state, not a flip"
        );
    }

    #[test]
    fn an_unparsable_live_configuration_is_refused() {
        let error = apply_change(
            "not = = valid toml",
            IndexScopeChange::SetContentIndex { enabled: true },
        )
        .expect_err("unparsable input must not produce a document");
        assert!(!error.reason.is_empty(), "{error}");
    }
}
