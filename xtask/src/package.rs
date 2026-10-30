// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Cairn release packaging automation (REL-01 ~ REL-04).
//! Builds a WiX v5 MSI and wraps it in the GPUI setup front end (`CairnSetup.exe`). The MSI remains
//! the transaction engine; the setup binary carries it as a verified payload and owns the visible
//! maintenance flow. Packaging is fail-closed: a missing WiX toolset, manifest tool, binary, or a
//! failed build leaves the previous release untouched.

use std::error::Error;
use std::fs;
use std::io::Write;
#[cfg(test)]
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const CAIRN_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const SETUP_EXE_NAME: &str = "CairnSetup.exe";
pub const UNINSTALLER_NAME: &str = "CairnUninstall.exe";
pub const MSI_NAME: &str = "cairn.msi";
pub const WIX_TEMPLATE_NAME: &str = "cairn.wxs";
pub const SHA256SUMS_NAME: &str = "SHA256SUMS.txt";
pub const INSTALLER_MANIFEST_NAME: &str = "cairn-setup.manifest";

/// Every file a successful package run publishes into the output directory, in publish order.
/// `SHA256SUMS.txt` is written last and covers the other entries by their final names.
const RELEASE_ARTIFACTS: [&str; 6] = [
    MSI_NAME,
    SETUP_EXE_NAME,
    WIX_TEMPLATE_NAME,
    ICON_NAME,
    LOGO_NAME,
    SHA256SUMS_NAME,
];

/// Fixed across releases: the MSI identity used for major upgrades.
pub const MSI_UPGRADE_CODE: &str = "4C65A8E1-7F89-4D12-9A4B-3D8F42D8E6C0";

pub const BINARIES: [&str; 6] = [
    "Cairn.exe",
    "CairnService.exe",
    "CairnCli.exe",
    "CairnWorker.exe",
    UNINSTALLER_NAME,
    SETUP_EXE_NAME,
];

/// Artifacts retired from the release surface. A successful publish removes them so `dist/`
/// describes exactly one current installer.
const OBSOLETE_ARTIFACTS: [&str; 4] = ["bundle.wxs", "theme.xml", "zh-CN.wxl", "cairn.yaml"];

const ICON_NAME: &str = "cairn.ico";
pub const LOGO_NAME: &str = "cairn-64.png";

/// Executes the full packaging workflow into `out_dir`.
///
/// Binaries are read from `CAIRN_BIN_DIR` when set, otherwise from `target/release` of the
/// workspace. The WiX executable is taken from `CAIRN_WIX` when set, otherwise from `PATH`.
pub fn run_package<P: AsRef<Path>>(out_dir: P) -> Result<(), Box<dyn Error>> {
    let workspace_root = workspace_root();
    let bin_dir = std::env::var_os("CAIRN_BIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target").join("release"));

    run_package_with(
        out_dir.as_ref(),
        &bin_dir,
        &wix_executable(),
        &mt_executable(),
    )
}

/// The WiX executable used for every build: `CAIRN_WIX` when set, otherwise `wix` from `PATH`.
fn wix_executable() -> PathBuf {
    std::env::var_os("CAIRN_WIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wix"))
}

/// The Windows SDK manifest tool. `CAIRN_MT` wins; otherwise `mt.exe` from PATH or the newest
/// installed Windows 10/11 SDK. `mt` replaces the GPUI-provided asInvoker manifest with the
/// setup's requireAdministrator manifest after linking; embedding a second manifest at link time
/// would duplicate the MANIFEST resource.
fn mt_executable() -> PathBuf {
    if let Some(path) = std::env::var_os("CAIRN_MT") {
        return PathBuf::from(path);
    }
    if let Ok(output) = Command::new("where.exe").arg("mt.exe").output() {
        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                let candidate = PathBuf::from(line.trim());
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    if let Some(kits) = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .map(|root| root.join("Windows Kits").join("10").join("bin"))
    {
        let mut versions: Vec<PathBuf> = fs::read_dir(&kits)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        versions.sort();
        for version in versions.iter().rev() {
            let candidate = version.join("x64").join("mt.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("mt.exe")
}

fn run_package_with(
    out_dir: &Path,
    bin_dir: &Path,
    wix: &Path,
    mt: &Path,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(out_dir)?;
    let out_dir = absolute_path(out_dir)?;
    let bin_dir = absolute_path(bin_dir)?;

    println!(
        "=== Packaging Cairn v{CAIRN_VERSION} to {} ===",
        out_dir.display()
    );

    check_binaries(&bin_dir)?;
    println!("Binaries: {}", bin_dir.display());

    check_wix(wix)?;
    println!("WiX: {}", wix.display());

    // Everything is produced in a private staging directory first. Only a complete release replaces
    // the files in `out_dir`; a failed build cannot delete the previous installer or leave a
    // half-written set behind.
    let staging_dir = create_staging_dir(&out_dir)?;
    let outcome = build_package(&staging_dir, &bin_dir, wix, mt)
        .and_then(|()| publish_package(&staging_dir, &out_dir));
    let succeeded = outcome.is_ok();
    let backup_remaining = staging_dir
        .join(".previous")
        .read_dir()
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if succeeded || !backup_remaining {
        let _ = fs::remove_dir_all(&staging_dir);
    } else {
        eprintln!(
            "warning: the previous release was not fully restored; kept {} for recovery",
            staging_dir.display()
        );
    }

    match outcome {
        Ok(()) => {
            println!("Packaging completed successfully.");
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Builds every release artifact inside `staging_dir`. Nothing outside that directory is touched.
fn build_package(
    staging_dir: &Path,
    bin_dir: &Path,
    wix: &Path,
    mt: &Path,
) -> Result<(), Box<dyn Error>> {
    // 1. Generate the WiX source and stage every input next to it.
    let wxs_path = staging_dir.join(WIX_TEMPLATE_NAME);
    generate_wix_template(&wxs_path)?;

    let icon_source = workspace_root()
        .join("assets")
        .join("icons")
        .join(ICON_NAME);
    if !icon_source.is_file() {
        return Err(format!("installer icon not found: {}", icon_source.display()).into());
    }
    fs::copy(&icon_source, staging_dir.join(ICON_NAME))?;

    let logo_source = workspace_root()
        .join("assets")
        .join("icons")
        .join(LOGO_NAME);
    if !logo_source.is_file() {
        return Err(format!("installer logo not found: {}", logo_source.display()).into());
    }
    fs::copy(&logo_source, staging_dir.join(LOGO_NAME))?;

    copy_binaries(bin_dir, staging_dir)?;

    let manifest_source = workspace_root()
        .join("crates")
        .join("cairn-setup-bin")
        .join("installer.manifest");
    if !manifest_source.is_file() {
        return Err(format!(
            "installer manifest not found: {}",
            manifest_source.display()
        )
        .into());
    }
    let manifest_path = staging_dir.join(INSTALLER_MANIFEST_NAME);
    fs::copy(&manifest_source, &manifest_path)?;

    // 2. The GPUI binaries already carry GPUI's own asInvoker manifest. Replace that resource with
    //    the setup's requireAdministrator manifest before WiX consumes the uninstaller.
    println!("Injecting installer manifest...");
    for name in [UNINSTALLER_NAME, SETUP_EXE_NAME] {
        inject_manifest(&staging_dir.join(name), mt, &manifest_path)?;
    }

    // 3. Compile the MSI. Every source is in the staging directory and resolved by relative name.
    println!("Building MSI...");
    let msi_path = staging_dir.join(MSI_NAME);
    let mut msi_cmd = Command::new(wix);
    msi_cmd
        .current_dir(staging_dir)
        .arg("build")
        .args(["-arch", "x64"])
        .args(["-pdbtype", "none"])
        .arg("-ext")
        .arg("WixToolset.Util.wixext")
        .arg("-bindpath")
        .arg(staging_dir)
        .arg("-o")
        .arg(&msi_path)
        .arg(&wxs_path);
    run_command(&mut msi_cmd, "WiX MSI build")?;
    require_artifact(&msi_path, "WiX MSI build")?;
    println!(
        "Built MSI: {} ({} bytes)",
        msi_path.display(),
        fs::metadata(&msi_path)?.len()
    );

    // 4. Append the MSI to the GPUI setup front end.
    let setup_path = staging_dir.join(SETUP_EXE_NAME);
    append_payload(&setup_path, &msi_path)?;
    println!(
        "Built installer: {} ({} bytes)",
        setup_path.display(),
        fs::metadata(&setup_path)?.len()
    );

    // 5. Generate SHA256SUMS.txt (REL-04) over the final file names.
    let sums_path = staging_dir.join(SHA256SUMS_NAME);
    generate_sha256sums(staging_dir, &sums_path)?;
    println!("Generated SHA256 checksums: {}", sums_path.display());

    Ok(())
}

fn copy_binaries(bin_dir: &Path, staging_dir: &Path) -> Result<(), Box<dyn Error>> {
    for name in BINARIES {
        let source = bin_dir.join(name);
        if !source.is_file() {
            return Err(format!("missing release binary: {}", source.display()).into());
        }
        fs::copy(&source, staging_dir.join(name))?;
    }
    Ok(())
}

fn inject_manifest(exe: &Path, mt: &Path, manifest: &Path) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new(mt);
    command
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .arg("-nologo")
        .arg("-manifest")
        .arg(manifest)
        .arg(format!("-outputresource:{};#1", exe.display()));
    run_command(&mut command, "installer manifest injection")
}

const PAYLOAD_MAGIC: &[u8; 8] = b"CAIRNMSI";
#[cfg(test)]
const PAYLOAD_FOOTER_LEN: u64 = 24;

/// Appends the MSI to the GPUI setup front end. The setup binary reads the same 24-byte footer at
/// startup, verifies the CRC and length, and extracts the MSI to a private temporary directory.
fn append_payload(exe: &Path, msi: &Path) -> Result<(), Box<dyn Error>> {
    let msi_bytes = fs::read(msi)?;
    if msi_bytes.is_empty() {
        return Err("refusing to embed an empty MSI".into());
    }
    if msi_bytes.len() > 512 * 1024 * 1024 {
        return Err("refusing to embed an MSI larger than the setup reader accepts".into());
    }
    let checksum = crc32fast::hash(&msi_bytes);
    let mut file = fs::OpenOptions::new().append(true).open(exe)?;
    file.write_all(&msi_bytes)?;
    file.write_all(&(msi_bytes.len() as u64).to_le_bytes())?;
    file.write_all(&checksum.to_le_bytes())?;
    file.write_all(&[0u8; 4])?;
    file.write_all(PAYLOAD_MAGIC)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
fn extract_payload(exe: &Path, msi: &Path) -> Result<(), Box<dyn Error>> {
    let mut file = fs::File::open(exe)?;
    let file_len = file.metadata()?.len();
    if file_len < PAYLOAD_FOOTER_LEN {
        return Err("setup executable has no payload".into());
    }
    file.seek(SeekFrom::Start(file_len - PAYLOAD_FOOTER_LEN))?;
    let mut footer = [0u8; PAYLOAD_FOOTER_LEN as usize];
    file.read_exact(&mut footer)?;
    if &footer[16..24] != PAYLOAD_MAGIC {
        return Err("setup executable payload footer is missing".into());
    }
    let length = u64::from_le_bytes(footer[0..8].try_into()?);
    let expected_crc = u32::from_le_bytes(footer[8..12].try_into()?);
    if length == 0 || length > file_len - PAYLOAD_FOOTER_LEN {
        return Err("setup executable payload length is invalid".into());
    }
    file.seek(SeekFrom::Start(file_len - PAYLOAD_FOOTER_LEN - length))?;
    let mut payload = Vec::with_capacity(length as usize);
    file.take(length).read_to_end(&mut payload)?;
    if payload.len() as u64 != length {
        return Err("setup executable payload is truncated".into());
    }
    if crc32fast::hash(&payload) != expected_crc {
        return Err("setup executable payload checksum does not match".into());
    }
    fs::write(msi, payload)?;
    Ok(())
}

/// Creates a unique build directory next to the final artifacts so publishing is a same-volume
/// rename. It is removed whether the build succeeds or fails.
fn create_staging_dir(out_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let staging_dir = out_dir.join(format!(".cairn-package-{}-{unique}", std::process::id()));
    fs::create_dir(&staging_dir).map_err(|err| {
        format!(
            "could not create staging directory {}: {err}",
            staging_dir.display()
        )
    })?;
    Ok(staging_dir)
}

/// Replaces the previous release in `out_dir` only after every new artifact exists. The previous
/// files are moved aside first, and restored if any rename fails, so a publish failure does not
/// destroy the release that was already there.
fn publish_package(staging_dir: &Path, out_dir: &Path) -> Result<(), Box<dyn Error>> {
    for name in RELEASE_ARTIFACTS {
        require_artifact(&staging_dir.join(name), "packaging")?;
    }

    let backup_dir = staging_dir.join(".previous");
    fs::create_dir(&backup_dir)?;

    let mut previous: Vec<&str> = Vec::new();
    for name in RELEASE_ARTIFACTS {
        let target = out_dir.join(name);
        if !target.exists() {
            continue;
        }
        if let Err(err) = fs::rename(&target, backup_dir.join(name)) {
            restore_previous(&backup_dir, out_dir, &previous);
            return Err(format!(
                "could not move the previous {} out of the way: {err}",
                target.display()
            )
            .into());
        }
        previous.push(name);
    }

    let mut published: Vec<&str> = Vec::new();
    for name in RELEASE_ARTIFACTS {
        let target = out_dir.join(name);
        if let Err(err) = fs::rename(staging_dir.join(name), &target) {
            for published_name in published.iter().rev() {
                let _ = fs::remove_file(out_dir.join(*published_name));
            }
            restore_previous(&backup_dir, out_dir, &previous);
            return Err(format!(
                "could not publish {}: {err}; the previous release was restored",
                target.display()
            )
            .into());
        }
        published.push(name);
    }

    for name in OBSOLETE_ARTIFACTS {
        let path = out_dir.join(name);
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }

    Ok(())
}

fn restore_previous(backup_dir: &Path, out_dir: &Path, names: &[&str]) {
    for name in names.iter().rev() {
        if let Err(err) = fs::rename(backup_dir.join(*name), out_dir.join(*name)) {
            eprintln!("warning: could not restore the previous {name}: {err}");
        }
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live inside the workspace")
        .to_path_buf()
}

/// Returns an absolute path without the `\\?\` extended-length prefix. WiX parses source and
/// output paths as URIs and rejects the extended-length form.
fn absolute_path(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn check_binaries(bin_dir: &Path) -> Result<(), Box<dyn Error>> {
    if !bin_dir.is_dir() {
        return Err(format!(
            "release binary directory not found: {} (build with `cargo build --release`)",
            bin_dir.display()
        )
        .into());
    }

    let missing: Vec<&str> = BINARIES
        .iter()
        .copied()
        .filter(|name| !bin_dir.join(name).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "missing release binaries in {}: {} (build with `cargo build --release`)",
            bin_dir.display(),
            missing.join(", ")
        )
        .into());
    }

    Ok(())
}

fn check_wix(wix: &Path) -> Result<(), Box<dyn Error>> {
    let output = Command::new(wix).arg("--version").output().map_err(|err| {
        format!(
            "WiX Toolset 5 executable `{}` could not be started: {err}. \
             Install WiX 5 (`dotnet tool install --global wix --version 5.0.2`) and ensure it is on PATH, \
             or set CAIRN_WIX to wix.exe.",
            wix.display()
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "WiX Toolset 5 executable `{}` failed `--version` ({}): {}",
            wix.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    Ok(())
}

fn require_artifact(path: &Path, what: &str) -> Result<(), Box<dyn Error>> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!(
            "{what} reported success but did not produce {}",
            path.display()
        )
        .into())
    }
}

fn run_command(command: &mut Command, what: &str) -> Result<(), Box<dyn Error>> {
    let output = command
        .output()
        .map_err(|err| format!("{what} could not be started: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{what} failed ({}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }

    Ok(())
}

fn generate_wix_template(wxs_path: &Path) -> Result<(), Box<dyn Error>> {
    let wxs_content = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs"
     xmlns:util="http://wixtoolset.org/schemas/v4/wxs/util">
  <Package Name="Cairn"
           Manufacturer="Cairn Contributors"
           Version="{version}"
           Language="1033"
           Scope="perMachine"
           Compressed="yes"
           UpgradeCode="{upgrade_code}">
    <MajorUpgrade Schedule="afterInstallInitialize"
                  DowngradeErrorMessage="A newer version of Cairn is already installed." />
    <MediaTemplate EmbedCab="yes" />
    <Icon Id="CairnIcon.ico" SourceFile="{icon}" />
    <Property Id="ARPPRODUCTICON" Value="CairnIcon.ico" />
    <!-- The MSI's own Apps & features entry stays hidden. The GPUI uninstaller owns the one
         visible entry below, so a person never sees two Cairn products or two uninstall paths. -->
    <Property Id="ARPSYSTEMCOMPONENT" Value="1" />

    <StandardDirectory Id="ProgramFiles64Folder">
      <Directory Id="INSTALLFOLDER" Name="Cairn">
        <Component Id="CairnGuiComponent" Guid="A1B2C3D4-E5F6-4A7B-8C9D-0E1F2A3B4C5D">
          <File Id="CairnExe" Source="Cairn.exe" KeyPath="yes">
            <Shortcut Id="ApplicationStartMenuShortcut"
                      Directory="ApplicationProgramsFolder"
                      Name="Cairn"
                      Description="Cairn Instant File Search"
                      WorkingDirectory="INSTALLFOLDER"
                      Icon="CairnIcon.ico"
                      Advertise="yes" />
            <Shortcut Id="ApplicationDesktopShortcut"
                      Directory="DesktopFolder"
                      Name="Cairn"
                      Description="Cairn Instant File Search"
                      WorkingDirectory="INSTALLFOLDER"
                      Icon="CairnIcon.ico"
                      Advertise="yes" />
          </File>
        </Component>

        <Component Id="CairnServiceComponent" Guid="B2C3D4E5-F6A7-4B8C-9D0E-1F2A3B4C5D6E">
          <File Id="CairnServiceExe" Source="CairnService.exe" KeyPath="yes" />
          <ServiceInstall Id="CairnServiceInstall"
                          Type="ownProcess"
                          Name="CairnService"
                          DisplayName="Cairn Service"
                          Description="High-performance background indexing service for Cairn."
                          Start="auto"
                          Account="LocalSystem"
                          ErrorControl="normal" />
          <ServiceControl Id="CairnServiceControl"
                          Name="CairnService"
                          Start="install"
                          Stop="both"
                          Remove="uninstall"
                          Wait="yes" />
          <!-- A crashed indexer must come back without a user opening the GUI and without UAC. The
               SCM restart policy is part of the installation, not a runtime preference. -->
          <util:ServiceConfig ServiceName="CairnService"
                              FirstFailureActionType="restart"
                              SecondFailureActionType="restart"
                              ThirdFailureActionType="restart"
                              RestartServiceDelayInSeconds="5"
                              ResetPeriodInDays="1" />
        </Component>

        <Component Id="CairnCliComponent" Guid="C3D4E5F6-A7B8-4C9D-0E1F-2A3B4C5D6E7F">
          <File Id="CairnCliExe" Source="CairnCli.exe" KeyPath="yes" />
        </Component>

        <Component Id="CairnWorkerComponent" Guid="D4E5F6A7-B8C9-4D0E-1F2A-3B4C5D6E7F80">
          <File Id="CairnWorkerExe" Source="CairnWorker.exe" KeyPath="yes" />
        </Component>

        <!-- The visible maintenance entry. The file is the GPUI uninstaller and the registry block
             is the only Cairn entry Apps & features shows. The cleanup helper removes the folder
             and the data outside the MSI after this process exits. -->
        <Component Id="CairnUninstallerComponent" Guid="F6A7B8C9-D0E1-4F2A-B3C4-D5E6F7A8B9C0">
          <File Id="CairnUninstallerExe" Source="{uninstaller}" KeyPath="yes" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="DisplayName" Type="string" Value="Cairn" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="DisplayVersion" Type="string" Value="[ProductVersion]" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="Publisher" Type="string" Value="Cairn Contributors" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="DisplayIcon" Type="string" Value="[INSTALLFOLDER]Cairn.exe,0" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="InstallLocation" Type="string" Value="[INSTALLFOLDER]" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="UninstallString" Type="string"
                         Value="&quot;[INSTALLFOLDER]CairnUninstall.exe&quot;" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="QuietUninstallString" Type="string"
                         Value="&quot;[INSTALLFOLDER]CairnUninstall.exe&quot; --quiet" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="NoModify" Type="integer" Value="1" />
          <RegistryValue Root="HKLM"
                         Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                         Name="NoRepair" Type="integer" Value="1" />
          <RegistryValue Root="HKLM" Key="Software\Cairn"
                         Name="Version" Type="string" Value="[ProductVersion]" />
          <RegistryValue Root="HKLM" Key="Software\Cairn"
                         Name="InstallFolder" Type="string" Value="[INSTALLFOLDER]" />
          <RegistryValue Root="HKLM" Key="Software\Cairn"
                         Name="ProductCode" Type="string" Value="[ProductCode]" />
          <RemoveRegistryKey Root="HKLM"
                             Key="Software\Microsoft\Windows\CurrentVersion\Uninstall\Cairn"
                             Action="removeOnUninstall" />
          <RemoveRegistryKey Root="HKLM" Key="Software\Cairn" Action="removeOnUninstall" />
        </Component>

        <!-- A real CLI must be callable by name. The machine PATH entry is removed with the
             component on uninstall. -->
        <Component Id="CairnPathComponent" Guid="A7B8C9D0-E1F2-4A3B-C4D5-E6F7A8B9C0D1">
          <Environment Id="CairnPathEntry"
                       Name="PATH"
                       Value="[INSTALLFOLDER]"
                       Part="last"
                       Action="set"
                       System="yes" />
          <RegistryValue Root="HKLM" Key="Software\Cairn"
                         Name="PathEntry" Type="integer" Value="1" KeyPath="yes" />
        </Component>

        <!-- Machine-wide key path and start-menu folder cleanup. Keeping this component under
             INSTALLFOLDER (rather than under the per-user ProgramMenuFolder) is what allows an
             HKLM key path in a perMachine package. -->
        <Component Id="CairnStartMenuComponent" Guid="E5F6A7B8-C9D0-4E1F-A2B3-C4D5E6F7A8B9">
          <RemoveFolder Id="RemoveApplicationProgramsFolder"
                        Directory="ApplicationProgramsFolder"
                        On="uninstall" />
          <RegistryValue Root="HKLM"
                         Key="Software\Cairn"
                         Name="StartMenuFolder"
                         Type="integer"
                         Value="1"
                         KeyPath="yes" />
        </Component>
      </Directory>
    </StandardDirectory>

    <!-- The shortcuts above install into this directory; it is only removed when empty. -->
    <StandardDirectory Id="ProgramMenuFolder">
      <Directory Id="ApplicationProgramsFolder" Name="Cairn" />
    </StandardDirectory>

    <Feature Id="MainApplication" Title="Cairn" Level="1">
      <ComponentRef Id="CairnGuiComponent" />
      <ComponentRef Id="CairnServiceComponent" />
      <ComponentRef Id="CairnCliComponent" />
      <ComponentRef Id="CairnWorkerComponent" />
      <ComponentRef Id="CairnUninstallerComponent" />
      <ComponentRef Id="CairnPathComponent" />
      <ComponentRef Id="CairnStartMenuComponent" />
    </Feature>

    <!-- The installed service is auto-start, but a crash or a manual stop must not leave a
         non-administrator unable to use the app. Grant Interactive Users SERVICE_START (RP) while
         keeping stop/config rights with SYSTEM and Administrators. -->
    <CustomAction Id="GrantInteractiveServiceStart"
                  Directory="SystemFolder"
                  ExeCommand="sc.exe sdset CairnService D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)"
                  Execute="deferred"
                  Impersonate="no"
                  Return="check" />
    <InstallExecuteSequence>
      <Custom Action="GrantInteractiveServiceStart"
              After="StartServices"
              Condition="NOT REMOVE~=&quot;ALL&quot;" />
    </InstallExecuteSequence>
  </Package>
</Wix>
"#,
        version = CAIRN_VERSION,
        upgrade_code = MSI_UPGRADE_CODE,
        icon = ICON_NAME,
        uninstaller = UNINSTALLER_NAME
    );

    fs::write(wxs_path, wxs_content)?;
    Ok(())
}

/// Hashes exactly the release artifacts, by their final file names, rather than every file that
/// happens to be in the directory. Stray files in `out_dir` never leak into the checksum list.
fn generate_sha256sums(staging_dir: &Path, sums_path: &Path) -> Result<(), Box<dyn Error>> {
    let mut entries = Vec::new();

    for file_name in RELEASE_ARTIFACTS {
        if file_name == SHA256SUMS_NAME {
            continue;
        }
        let file_path = staging_dir.join(file_name);
        require_artifact(&file_path, "SHA-256 manifest")?;
        let bytes = fs::read(&file_path)?;
        let digest = sha256_digest(&bytes);
        entries.push(format!("{}  {}", digest, file_name));
    }

    let sums_content = entries.join("\n") + "\n";
    fs::write(sums_path, sums_content)?;
    Ok(())
}

/// Computes SHA-256 hash using standard FIPS 180-2 constants and logic.
#[allow(clippy::chunks_exact_to_as_chunks)]
pub fn sha256_digest(data: &[u8]) -> String {
    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut h_val = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h_val
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h_val = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(h_val);
    }

    let mut hex = String::with_capacity(64);
    for val in h {
        hex.push_str(&format!("{:08x}", val));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fake_binaries(bin_dir: &Path) {
        fs::create_dir_all(bin_dir).expect("create bin dir");
        for name in BINARIES {
            fs::write(bin_dir.join(name), b"MZ fake binary").expect("write fake binary");
        }
    }

    fn fake_mt(dir: &Path) -> PathBuf {
        let path = dir.join("fake-mt.cmd");
        let script = ["@echo off", "exit /b 0", ""].join("\r\n");
        fs::write(&path, script).expect("write fake mt");
        path
    }

    fn staging_entries(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .expect("read output directory")
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with(".cairn-package-"))
            .collect()
    }

    fn write_old_release(dist: &Path) -> (Vec<u8>, Vec<u8>) {
        fs::create_dir_all(dist).expect("create dist");
        let old_msi = b"previous MSI".to_vec();
        let old_setup = b"previous CairnSetup.exe".to_vec();
        fs::write(dist.join(MSI_NAME), &old_msi).expect("write old MSI");
        fs::write(dist.join(SETUP_EXE_NAME), &old_setup).expect("write old setup");
        (old_msi, old_setup)
    }

    fn assert_old_release(dist: &Path, old_msi: &[u8], old_setup: &[u8]) {
        assert_eq!(
            fs::read(dist.join(MSI_NAME)).expect("old MSI must still exist"),
            old_msi,
            "a failed package run must not delete or replace the previous MSI"
        );
        assert_eq!(
            fs::read(dist.join(SETUP_EXE_NAME)).expect("old setup must still exist"),
            old_setup,
            "a failed package run must not delete or replace the previous setup"
        );
        assert!(
            staging_entries(dist).is_empty(),
            "staging directories must be removed after a failure: {:?}",
            staging_entries(dist)
        );
    }

    #[test]
    fn test_sha256_nist_vectors() {
        // NIST test vector: empty string
        assert_eq!(
            sha256_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        // NIST test vector: "abc"
        assert_eq!(
            sha256_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn missing_wix_returns_error_and_preserves_the_previous_release() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let bin_dir = temp_dir.path().join("bin");
        write_fake_binaries(&bin_dir);

        let dist = temp_dir.path().join("dist");
        let (old_msi, old_setup) = write_old_release(&dist);

        let err = run_package_with(
            &dist,
            &bin_dir,
            Path::new("cairn-wix-does-not-exist.exe"),
            Path::new("mt.exe"),
        )
        .expect_err("packaging must fail when WiX is unavailable");

        assert!(err.to_string().contains("WiX"), "unexpected error: {err}");
        assert_old_release(&dist, &old_msi, &old_setup);
    }

    /// A WiX stand-in that passes `--version` (so the toolset check succeeds) and then fails every
    /// build. This exercises a failure after the staging directory exists, not just the up-front
    /// tool checks.
    #[test]
    fn failed_builds_preserve_the_previous_release() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let bin_dir = temp_dir.path().join("bin");
        write_fake_binaries(&bin_dir);

        let fake_wix = temp_dir.path().join("fake-wix.cmd");
        fs::write(
            &fake_wix,
            "@echo off\r\n\
             if \"%~1\"==\"--version\" (\r\n\
               echo 5.0.2\r\n\
               exit /b 0\r\n\
             )\r\n\
             echo simulated WiX build failure 1>&2\r\n\
             exit /b 1\r\n",
        )
        .expect("write fake WiX");

        let dist = temp_dir.path().join("dist");
        let (old_msi, old_setup) = write_old_release(&dist);

        let err = run_package_with(&dist, &bin_dir, &fake_wix, &fake_mt(temp_dir.path()))
            .expect_err("a failing WiX build must fail packaging");

        assert!(
            err.to_string().contains("WiX MSI build"),
            "unexpected error: {err}"
        );
        assert_old_release(&dist, &old_msi, &old_setup);
    }

    #[test]
    fn failing_wix_version_check_returns_error() {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let failing_exe = PathBuf::from(system_root)
            .join("System32")
            .join("where.exe");
        if !failing_exe.is_file() {
            return;
        }

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let bin_dir = temp_dir.path().join("bin");
        write_fake_binaries(&bin_dir);

        // where.exe rejects `--version` with a non-zero exit code, simulating a broken WiX install.
        let err = run_package_with(
            &temp_dir.path().join("dist"),
            &bin_dir,
            &failing_exe,
            Path::new("mt.exe"),
        )
        .expect_err("a failing WiX process must fail packaging");

        assert!(err.to_string().contains("WiX"), "unexpected error: {err}");
    }

    #[test]
    fn missing_binaries_return_error() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let err = run_package_with(
            &temp_dir.path().join("dist"),
            &temp_dir.path().join("empty-bin"),
            Path::new("wix"),
            Path::new("mt.exe"),
        )
        .expect_err("packaging must fail when release binaries are missing");

        assert!(
            err.to_string().contains("release binary directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn checksums_cover_exactly_the_release_artifacts_by_final_name() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let staging = temp_dir.path().join("staging");
        fs::create_dir_all(&staging).expect("create staging");

        for name in RELEASE_ARTIFACTS {
            if name != SHA256SUMS_NAME {
                fs::write(staging.join(name), format!("content of {name}"))
                    .expect("write artifact");
            }
        }
        // Stray files from an earlier run must not leak into the release checksum list.
        fs::write(staging.join("unrelated.txt"), b"not part of the release").expect("write stray");

        let sums_path = staging.join(SHA256SUMS_NAME);
        generate_sha256sums(&staging, &sums_path).expect("generate checksums");
        let sums = fs::read_to_string(&sums_path).expect("read checksums");

        let lines: Vec<&str> = sums.lines().collect();
        assert_eq!(lines.len(), RELEASE_ARTIFACTS.len() - 1);
        for name in RELEASE_ARTIFACTS {
            if name == SHA256SUMS_NAME {
                continue;
            }
            let digest = sha256_digest(format!("content of {name}").as_bytes());
            assert!(
                lines.contains(&format!("{digest}  {name}").as_str()),
                "missing checksum for final file name {name}: {sums}"
            );
        }
        assert!(!sums.contains("unrelated.txt"));
    }

    #[test]
    fn publishing_replaces_old_artifacts_with_the_complete_new_set() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let staging = temp_dir.path().join("staging");
        let dist = temp_dir.path().join("dist");
        fs::create_dir_all(&staging).expect("create staging");
        fs::create_dir_all(&dist).expect("create dist");

        let (old_msi, old_setup) = write_old_release(&dist);
        for name in RELEASE_ARTIFACTS {
            fs::write(staging.join(name), format!("new {name}")).expect("write new artifact");
        }

        publish_package(&staging, &dist).expect("publish");

        assert_eq!(
            fs::read(dist.join(MSI_NAME)).expect("new MSI"),
            b"new cairn.msi".to_vec()
        );
        assert_eq!(
            fs::read(dist.join(SETUP_EXE_NAME)).expect("new setup"),
            b"new CairnSetup.exe".to_vec()
        );
        assert_ne!(
            fs::read(dist.join(MSI_NAME)).expect("new MSI"),
            old_msi,
            "the previous MSI must be replaced"
        );
        assert_ne!(
            fs::read(dist.join(SETUP_EXE_NAME)).expect("new setup"),
            old_setup,
            "the previous setup must be replaced"
        );
        for name in RELEASE_ARTIFACTS {
            assert!(
                dist.join(name).is_file(),
                "published artifact {name} is missing"
            );
        }
    }

    /// Reads MSI Summary Information property 7 (the platform/language template, e.g. `x64;1033`).
    fn msi_summary_template(msi: &Path) -> String {
        const SCRIPT: &str = "$installer = New-Object -ComObject WindowsInstaller.Installer; \
            $si = $installer.GetType().InvokeMember('SummaryInformation','GetProperty',$null,$installer,@($env:CAIRN_MSI,0)); \
            $si.GetType().InvokeMember('Property','GetProperty',$null,$si,@(7))";
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
            .env("CAIRN_MSI", msi)
            .output()
            .expect("start PowerShell to read the MSI summary");
        assert!(
            output.status.success(),
            "reading the MSI summary failed:\n{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Real packaging needs WiX 5, the Windows SDK manifest tool, and release binaries, so it is
    /// ignored by default. Run with:
    /// `cargo test -p xtask real_package_builds_msi_and_setup -- --ignored`
    #[test]
    #[ignore = "requires WiX 5, mt.exe, and release binaries in target/release"]
    fn real_package_builds_msi_and_setup() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let dist = temp_dir.path().join("dist");

        run_package(&dist).expect("real packaging");

        for name in RELEASE_ARTIFACTS {
            assert!(
                dist.join(name).is_file(),
                "packaging did not produce {name}"
            );
        }
        assert!(
            staging_entries(&dist).is_empty(),
            "staging directories were left behind: {:?}",
            staging_entries(&dist)
        );
        for obsolete in OBSOLETE_ARTIFACTS {
            assert!(
                !dist.join(obsolete).exists(),
                "the retired artifact {obsolete} must not remain in dist/"
            );
        }

        let msi_path = dist.join(MSI_NAME);
        let setup_path = dist.join(SETUP_EXE_NAME);
        let sums = fs::read_to_string(dist.join(SHA256SUMS_NAME)).expect("read checksums");
        assert_eq!(sums.lines().count(), RELEASE_ARTIFACTS.len() - 1);
        for name in RELEASE_ARTIFACTS {
            if name != SHA256SUMS_NAME {
                assert!(
                    sums.lines()
                        .any(|line| line.ends_with(&format!("  {name}"))),
                    "checksum manifest is missing {name}"
                );
            }
        }

        let wix = wix_executable();

        // The MSI database must pass WiX's ICE validation.
        let mut validate = Command::new(&wix);
        validate.args(["msi", "validate"]).arg(&msi_path);
        run_command(&mut validate, "WiX MSI validation").expect("MSI must pass `wix msi validate`");

        // The setup binary carries the exact MSI that was built, with a checked payload footer.
        let extracted_msi = temp_dir.path().join("payload.msi");
        extract_payload(&setup_path, &extracted_msi).expect("extract setup payload");
        assert_eq!(
            sha256_digest(&fs::read(&extracted_msi).expect("read extracted MSI")),
            sha256_digest(&fs::read(&msi_path).expect("read built MSI")),
            "the MSI inside CairnSetup.exe differs from cairn.msi"
        );

        // The GPUI setup carries the setup manifest resource, not GPUI's asInvoker default. The
        // resource is plain XML in the PE, so the assertion does not need a second tool run on a
        // freshly written executable that Defender may still be scanning.
        let setup_bytes = fs::read(&setup_path).expect("read setup executable");
        let setup_text = String::from_utf8_lossy(&setup_bytes);
        assert!(
            setup_text.contains("requireAdministrator"),
            "CairnSetup.exe does not request elevation"
        );
        assert!(
            setup_text.contains("PerMonitorV2"),
            "CairnSetup.exe does not declare per-monitor DPI awareness"
        );

        // Decompiling proves the package contains the service, the single visible uninstall entry,
        // the machine PATH entry, and no retired bundle artifact.
        let decompiled_path = temp_dir.path().join("decompiled.wxs");
        let mut decompile = Command::new(&wix);
        decompile
            .args(["msi", "decompile"])
            .arg(&msi_path)
            .arg("-o")
            .arg(&decompiled_path);
        run_command(&mut decompile, "WiX MSI decompilation")
            .expect("MSI must be decompilable with `wix msi decompile`");
        let decompiled = fs::read_to_string(&decompiled_path).expect("read decompiled MSI");

        assert!(decompiled.contains("<ServiceInstall"));
        assert!(decompiled.contains("<ServiceControl"));
        assert!(decompiled.contains("Start=\"auto\""));
        assert!(
            decompiled.contains("Start=\"install\"")
                && decompiled.contains("Account=\"LocalSystem\"")
        );
        assert!(decompiled.contains("GrantInteractiveServiceStart"));
        assert!(decompiled.contains("Wix4ServiceConfig"));
        assert!(decompiled.contains("Directory=\"DesktopFolder\""));
        assert!(decompiled.contains("Directory=\"ApplicationProgramsFolder\""));
        assert!(decompiled.contains("ARPSYSTEMCOMPONENT"));
        assert!(decompiled.contains("CairnUninstall.exe"));
        assert!(decompiled.contains("UninstallString"));
        assert!(decompiled.contains("QuietUninstallString"));
        assert!(decompiled.contains("<Environment"));
        assert!(
            decompiled.contains("ForceDeleteOnUninstall")
                || decompiled.contains("RemoveRegistryKey")
        );
        assert!(decompiled.contains("Root=\"HKLM\""));
        assert!(!decompiled.contains("Root=\"HKCU\""));
        let x64_components = decompiled.matches("Bitness=\"always64\"").count();
        assert!(
            x64_components >= 7,
            "expected every component to be x64, found {x64_components} always64 components"
        );

        let summary = msi_summary_template(&msi_path);
        assert!(
            summary.starts_with("x64"),
            "MSI summary template must target x64, got {summary:?}"
        );
    }
}
