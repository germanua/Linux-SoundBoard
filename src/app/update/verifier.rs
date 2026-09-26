use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use super::UpdateError;

#[cfg(lsb_dev_profile)]
const RELEASE_KEYRING: &str = include_str!("../../../dev/release-keyring.txt");
#[cfg(not(lsb_dev_profile))]
const RELEASE_KEYRING: &str = include_str!("../../../release-keyring.txt");
const MAX_TRUSTED_RELEASE_KEYS: usize = 4;

fn trusted_release_keys(keyring: &str) -> Result<Vec<PublicKey>, UpdateError> {
    let entries = keyring
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if entries.is_empty() || entries.len() > MAX_TRUSTED_RELEASE_KEYS {
        return Err(UpdateError::Verification(
            "invalid embedded release keyring".into(),
        ));
    }
    entries
        .into_iter()
        .map(|entry| {
            PublicKey::from_base64(entry)
                .map_err(|error| UpdateError::Verification(error.to_string()))
        })
        .collect()
}

fn verify_manifest_with_keyring(
    manifest: &[u8],
    signature_text: &str,
    tag: &str,
    keyring: &str,
) -> Result<(), UpdateError> {
    let signature = Signature::decode(signature_text)
        .map_err(|error| UpdateError::Verification(error.to_string()))?;
    let trusted = trusted_release_keys(keyring)?
        .into_iter()
        .any(|public_key| public_key.verify(manifest, &signature, false).is_ok());
    if !trusted {
        return Err(UpdateError::Verification(
            "release signature is not signed by a trusted key".into(),
        ));
    }
    let expected = format!("Linux Soundboard release {tag}");
    if signature.trusted_comment() != expected {
        return Err(UpdateError::Verification(format!(
            "release signature is bound to '{}', expected '{expected}'",
            signature.trusted_comment()
        )));
    }
    Ok(())
}

pub fn verify_manifest(
    manifest: &[u8],
    signature_text: &str,
    tag: &str,
) -> Result<(), UpdateError> {
    verify_manifest_with_keyring(manifest, signature_text, tag, RELEASE_KEYRING)
}

pub fn parse_manifest(manifest: &[u8]) -> Result<BTreeMap<String, String>, UpdateError> {
    let text = std::str::from_utf8(manifest)
        .map_err(|error| UpdateError::Verification(error.to_string()))?;
    let mut entries = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (hash, name) = line
            .split_once("  ")
            .ok_or_else(|| UpdateError::Verification("invalid checksum manifest line".into()))?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(UpdateError::Verification("invalid SHA-256 value".into()));
        }
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(UpdateError::Verification(
                "unsafe manifest asset name".into(),
            ));
        }
        if entries
            .insert(name.to_string(), hash.to_ascii_lowercase())
            .is_some()
        {
            return Err(UpdateError::Verification("duplicate manifest asset".into()));
        }
    }
    if entries.is_empty() {
        return Err(UpdateError::Verification("empty checksum manifest".into()));
    }
    Ok(entries)
}

pub fn verify_bytes(
    name: &str,
    data: &[u8],
    entries: &BTreeMap<String, String>,
) -> Result<(), UpdateError> {
    let expected = entries.get(name).ok_or_else(|| {
        UpdateError::Verification(format!("{name} is missing from the signed manifest"))
    })?;
    let actual = format!("{:x}", Sha256::digest(data));
    if &actual != expected {
        return Err(UpdateError::Verification(format!(
            "SHA-256 mismatch for {name}"
        )));
    }
    Ok(())
}

pub fn verify_file(
    name: &str,
    path: &std::path::Path,
    entries: &BTreeMap<String, String>,
) -> Result<(), UpdateError> {
    use std::io::Read;

    let expected = entries.get(name).ok_or_else(|| {
        UpdateError::Verification(format!("{name} is missing from the signed manifest"))
    })?;
    let mut file =
        std::fs::File::open(path).map_err(|error| UpdateError::State(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| UpdateError::State(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if &actual != expected {
        return Err(UpdateError::Verification(format!(
            "SHA-256 mismatch for {name}"
        )));
    }
    Ok(())
}

#[cfg(all(test, not(lsb_dev_profile)))]
mod tests {
    use super::*;

    #[test]
    fn project_release_signature_verifies_in_process() {
        let manifest = include_bytes!("../../tests/fixtures/update-signature/SHA256SUMS.txt");
        let signature =
            include_str!("../../tests/fixtures/update-signature/SHA256SUMS.txt.minisig");
        verify_manifest(manifest, signature, "v2.4.5").unwrap();
        assert!(verify_manifest(manifest, signature, "v2.4.6").is_err());
    }

    #[test]
    fn overlapping_keyring_accepts_the_matching_trusted_key() {
        let manifest = include_bytes!("../../tests/fixtures/update-signature/SHA256SUMS.txt");
        let signature =
            include_str!("../../tests/fixtures/update-signature/SHA256SUMS.txt.minisig");
        let unrelated = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let current = include_str!("../../../release-keyring.txt").trim();
        let keyring = format!("{unrelated}\n{current}\n");
        verify_manifest_with_keyring(manifest, signature, "v2.4.5", &keyring).unwrap();
        assert!(verify_manifest_with_keyring(manifest, signature, "v2.4.5", unrelated).is_err());
    }
}
