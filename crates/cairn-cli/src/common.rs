// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use cairn_core::property_store::SourceId;
use cairn_core::sort::SortSpec;
use cairn_protocol::message::{Envelope, QueryDefaults, ResponsePayload};
use clap::Subcommand;
use std::path::PathBuf;

pub(super) const CLI_PAGE_SIZE: usize = 500;
pub(super) const MAX_CLI_SNIPPETS: usize = 3;

#[derive(Clone, Debug)]
pub(super) enum TransportError {
    Connect(String),
    Request(String),
}

pub(super) trait CliTransport {
    fn get_status(&self) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn query(
        &self,
        query_id: u64,
        raw_query: &str,
        defaults: QueryDefaults,
        sort_spec: &SortSpec,
        first_page_limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn get_rows(
        &self,
        session_id: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn get_sources(&self) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn add_folder_source(&self, path: &str) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn remove_folder_source(&self, path: &str)
    -> Result<Envelope<ResponsePayload>, TransportError>;

    fn rescan(&self, volume_id: Option<u16>) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn rescan_source(
        &self,
        source_id: SourceId,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn reload_service_config(&self) -> Result<(bool, String), TransportError>;

    fn get_property_index_status(&self) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn set_property_index_enabled(
        &self,
        enabled: bool,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn get_index_journal(
        &self,
        after_seq: u64,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn set_index_journal_enabled(
        &self,
        enabled: bool,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;
}

fn parse_volume_letter(value: &str) -> Result<char, String> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii_alphabetic() => Ok(ch.to_ascii_uppercase()),
        _ => Err(format!(
            "invalid volume letter `{value}`; expected a single ASCII drive letter such as C"
        )),
    }
}

#[derive(Subcommand, Debug)]
pub(super) enum CliCommand {
    /// List indexed NTFS volumes and folder sources
    Sources,
    /// Register and scan a folder source
    AddFolder { path: String },
    /// Stop watching and remove a folder source
    RemoveFolder { path: String },
    /// Trigger an asynchronous index rescan
    Rescan {
        /// Drive letter to rescan; rescans all sources when omitted
        #[arg(long, value_parser = parse_volume_letter)]
        volume: Option<char>,
    },
    /// Trigger a rescan for one source by numeric ID
    RescanSource { source_id: u32 },
    /// Reload service configuration from service.toml
    ReloadConfig,
    /// Query or toggle the optional extended-property index
    PropertyIndex {
        /// Enable extended-property indexing
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        /// Disable extended-property indexing and release its side tables
        #[arg(long)]
        disable: bool,
    },
    /// Inspect the bounded index change journal or toggle recording
    Journal {
        /// Return entries with sequence strictly greater than this cursor
        #[arg(long, default_value_t = 0)]
        after_seq: u64,
        /// Maximum entries to return
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Enable index journal recording
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        /// Disable index journal recording
        #[arg(long)]
        disable: bool,
        /// Write the journal window after the given sequence number to a JSON file (all pages)
        #[arg(long, value_name = "PATH", conflicts_with_all = ["enable", "disable", "json"])]
        export: Option<PathBuf>,
    },
}
