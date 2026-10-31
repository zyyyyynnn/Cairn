// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The installer payload appended to `CairnSetup.exe`.
//!
//! The file is one signed executable on the release page: the GPUI setup front end, followed by the
//! MSI and a fixed footer. The footer is deliberately small and self-describing so the setup binary
//! can find its engine without a second file or a self-extraction directory format.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"CAIRNMSI";
const FOOTER_LEN: u64 = 24;
const MAX_PAYLOAD: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    #[error("the executable has no Cairn MSI payload")]
    Missing,
    #[error("the Cairn MSI payload footer is damaged")]
    CorruptFooter,
    #[error("the Cairn MSI payload is larger than this setup accepts")]
    TooLarge,
    #[error("the executable already carries a Cairn MSI payload")]
    AlreadyEmbedded,
    #[error("the Cairn MSI payload checksum does not match")]
    Checksum,
    #[error("I/O while handling the Cairn MSI payload: {0}")]
    Io(#[from] std::io::Error),
}

/// Appends `msi` to `exe` and writes the footer. The executable must not already carry a payload.
pub fn append_payload(exe: &Path, msi: &Path) -> Result<(), PayloadError> {
    if has_payload(exe)? {
        return Err(PayloadError::AlreadyEmbedded);
    }
    let msi_bytes = std::fs::read(msi)?;
    if msi_bytes.is_empty() {
        return Err(PayloadError::Missing);
    }
    if msi_bytes.len() as u64 > MAX_PAYLOAD {
        return Err(PayloadError::TooLarge);
    }
    let checksum = crc32fast::hash(&msi_bytes);

    let mut file = std::fs::OpenOptions::new().append(true).open(exe)?;
    file.write_all(&msi_bytes)?;
    file.write_all(&(msi_bytes.len() as u64).to_le_bytes())?;
    file.write_all(&checksum.to_le_bytes())?;
    file.write_all(&[0u8; 4])?;
    file.write_all(MAGIC)?;
    file.sync_all()?;
    Ok(())
}

/// Returns whether the file ends with a plausible payload footer.
pub fn has_payload(exe: &Path) -> Result<bool, PayloadError> {
    let mut file = File::open(exe)?;
    let len = file.metadata()?.len();
    if len < FOOTER_LEN {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(len - FOOTER_LEN))?;
    let mut footer = [0u8; FOOTER_LEN as usize];
    file.read_exact(&mut footer)?;
    Ok(&footer[16..24] == MAGIC)
}

/// Extracts the payload into `destination`, verifying its length and checksum.
pub fn extract_payload(exe: &Path, destination: &Path) -> Result<(), PayloadError> {
    let mut file = File::open(exe)?;
    let file_len = file.metadata()?.len();
    if file_len < FOOTER_LEN {
        return Err(PayloadError::Missing);
    }
    file.seek(SeekFrom::Start(file_len - FOOTER_LEN))?;
    let mut footer = [0u8; FOOTER_LEN as usize];
    file.read_exact(&mut footer)?;
    if &footer[16..24] != MAGIC {
        return Err(PayloadError::Missing);
    }
    let length = u64::from_le_bytes(footer[0..8].try_into().expect("eight-byte length"));
    let expected_crc = u32::from_le_bytes(footer[8..12].try_into().expect("four-byte CRC"));
    if length == 0 || length > MAX_PAYLOAD || length > file_len - FOOTER_LEN {
        return Err(PayloadError::CorruptFooter);
    }

    file.seek(SeekFrom::Start(file_len - FOOTER_LEN - length))?;
    let mut reader = file.take(length);
    let mut writer = File::create(destination)?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut copied = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        writer.write_all(&buffer[..read])?;
        copied += read as u64;
    }
    writer.sync_all()?;
    if copied != length {
        return Err(PayloadError::CorruptFooter);
    }
    if hasher.finalize() != expected_crc {
        return Err(PayloadError::Checksum);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{append_payload, extract_payload, has_payload, PayloadError};

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("CairnSetup.exe");
        let msi = dir.path().join("cairn.msi");
        std::fs::write(&exe, b"MZ fake setup executable").expect("exe");
        std::fs::write(&msi, b"MSI payload bytes").expect("msi");
        (dir, exe, msi)
    }

    #[test]
    fn a_payload_round_trips_byte_for_byte() {
        let (dir, exe, msi) = fixture();
        append_payload(&exe, &msi).expect("append");
        assert!(has_payload(&exe).expect("probe"));

        let extracted = dir.path().join("extracted.msi");
        extract_payload(&exe, &extracted).expect("extract");
        assert_eq!(
            std::fs::read(&extracted).expect("read extracted"),
            std::fs::read(&msi).expect("read source")
        );
    }

    #[test]
    fn an_executable_without_a_footer_is_not_mistaken_for_a_payload() {
        let (dir, exe, _) = fixture();
        assert!(!has_payload(&exe).expect("probe"));
        let extracted = dir.path().join("out.msi");
        assert!(matches!(
            extract_payload(&exe, &extracted),
            Err(PayloadError::Missing)
        ));
    }

    #[test]
    fn a_corrupt_payload_fails_its_checksum() {
        let (dir, exe, msi) = fixture();
        append_payload(&exe, &msi).expect("append");
        let mut bytes = std::fs::read(&exe).expect("read exe");
        let payload_start = bytes.len() - 24 - b"MSI payload bytes".len();
        bytes[payload_start] ^= 0xff;
        std::fs::write(&exe, bytes).expect("write corrupt exe");

        let extracted = dir.path().join("out.msi");
        assert!(matches!(
            extract_payload(&exe, &extracted),
            Err(PayloadError::Checksum)
        ));
    }

    #[test]
    fn a_second_append_is_refused() {
        let (_dir, exe, msi) = fixture();
        append_payload(&exe, &msi).expect("first append");
        assert!(matches!(
            append_payload(&exe, &msi),
            Err(PayloadError::AlreadyEmbedded)
        ));
    }
}
