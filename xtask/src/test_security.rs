// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Security and access control integration test suite (SPEC SVC-02, SET-02).

pub fn run_security_tests() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Cairn Security & Access Control Validation (SPEC SVC-02, SET-02) ===");

    // 1. Validate Default SDDL String
    let default_sddl = cairn_service::security::DEFAULT_SDDL;
    println!("Validating Default SDDL: {}", default_sddl);
    assert!(default_sddl.contains("SY"), "Must grant LocalSystem");
    assert!(default_sddl.contains("BA"), "Must grant Administrators");
    assert!(default_sddl.contains("IU"), "Must grant Interactive Users");
    assert!(
        !default_sddl.contains("NU"),
        "Must not grant Network Users (remote access blocked)"
    );
    assert!(
        !default_sddl.contains("WD"),
        "Must not grant Everyone/World"
    );

    // 2. Validate Admin-Only SDDL String
    let admin_sddl = cairn_service::security::ADMIN_ONLY_SDDL;
    println!("Validating Admin-Only SDDL: {}", admin_sddl);
    assert!(admin_sddl.contains("SY"));
    assert!(admin_sddl.contains("BA"));
    assert!(
        !admin_sddl.contains("IU"),
        "Admin-only mode must revoke Interactive User access"
    );

    #[cfg(windows)]
    {
        println!("Checking Win32 Security Descriptor conversion...");
        let desc = cairn_service::security::SafeSecurityDescriptor::create(false)
            .expect("Should construct default Win32 Security Descriptor");
        assert!(!desc.sa.lpSecurityDescriptor.is_null());

        let admin_desc = cairn_service::security::SafeSecurityDescriptor::create(true)
            .expect("Should construct admin-only Win32 Security Descriptor");
        assert!(!admin_desc.sa.lpSecurityDescriptor.is_null());
        println!("Security Descriptors parsed and validated by Windows kernel.");
    }

    println!("All security and DACL permissions tests passed successfully.");
    Ok(())
}
