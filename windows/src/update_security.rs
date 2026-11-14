//! Verification of the signed update manifest and the exact installer bytes.
//!
//! The detached Ed25519 signature covers the complete UTF-8 manifest, including
//! its version and trailing newline. No network metadata is trusted as a digest.

use std::io::{Read, Seek, SeekFrom};

use ring::{digest, signature};

pub const MAX_INSTALLER_SIZE: u64 = 512 * 1024 * 1024;
const HEADER: &str = "VelvetFrost update manifest v1";
const INSTALLER: &str = "VelvetFrost-Setup.exe";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub size: u64,
    digest: [u8; 32],
}

/// Strict three-component stable version. Reject aliases, suffixes and overflow.
fn version(text: &str) -> Option<[u32; 3]> {
    let mut parts = text.split('.');
    let mut value = [0; 3];
    for part in &mut value {
        let s = parts.next()?;
        if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *part = s.parse().ok()?;
    }
    parts.next().is_none().then_some(value)
}

/// Check the GitHub tag without accepting prereleases or version aliases.
pub fn newer_tag(tag: &str, current: &str) -> Result<bool, &'static str> {
    let candidate = tag.strip_prefix('v').and_then(version).ok_or("Invalid release tag")?;
    let installed = version(current).ok_or("Invalid installed version")?;
    Ok(candidate > installed)
}

fn hex_digest(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Verify before parsing, then bind all release identity fields to the signature.
pub fn verify_manifest(bytes: &[u8], signature_bytes: &[u8], public_key: &[u8], tag: &str, current: &str) -> Result<Manifest, &'static str> {
    if bytes.len() > 1024 || signature_bytes.len() != 64 || public_key.len() != 32 {
        return Err("Invalid update signature or manifest size");
    }
    signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
        .verify(bytes, signature_bytes)
        .map_err(|_| "Update signature verification failed")?;
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid update manifest")?;
    let fields: Vec<_> = text.split('\n').collect();
    if fields.len() != 6 || fields[0] != HEADER || fields[2] != INSTALLER || !fields[5].is_empty() {
        return Err("Invalid update manifest");
    }
    let candidate = version(fields[1]).ok_or("Invalid signed version")?;
    let installed = version(current).ok_or("Invalid installed version")?;
    if tag != format!("v{}", fields[1]) || candidate <= installed {
        return Err("Signed version does not match the release or is not newer");
    }
    let size_text = fields[3];
    if size_text.is_empty() || (size_text.len() > 1 && size_text.starts_with('0')) || !size_text.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid installer size");
    }
    let size: u64 = size_text.parse().map_err(|_| "Invalid installer size")?;
    if size == 0 || size > MAX_INSTALLER_SIZE {
        return Err("Invalid installer size");
    }
    let digest = hex_digest(fields[4]).ok_or("Invalid installer digest")?;
    Ok(Manifest { version: fields[1].to_owned(), size, digest })
}

/// Hash the same open file that will be protected against modification until launch.
pub fn verify_installer(file: &mut (impl Read + Seek), manifest: &Manifest) -> Result<(), &'static str> {
    file.seek(SeekFrom::Start(0)).map_err(|_| "Could not seek update file")?;
    let mut digest = digest::Context::new(&digest::SHA256);
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|_| "Could not read update file")?;
        if count == 0 {
            break;
        }
        total = total.checked_add(count as u64).ok_or("Invalid installer size")?;
        if total > manifest.size {
            return Err("Installer size does not match signed manifest");
        }
        digest.update(&buffer[..count]);
    }
    if total != manifest.size || digest.finish().as_ref() != manifest.digest {
        return Err("Installer contents do not match signed manifest");
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| "Could not seek update file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;
    use std::io::Cursor;

    fn signed(payload: &[u8], version: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let key = signature::Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
        let hash = digest::digest(&digest::SHA256, payload);
        let hex: String = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let bytes = format!("{HEADER}\n{version}\n{INSTALLER}\n{}\n{hex}\n", payload.len()).into_bytes();
        let sig = key.sign(&bytes).as_ref().to_vec();
        (bytes, sig, key.public_key().as_ref().to_vec())
    }

    #[test]
    fn signature_binds_version_and_contents() {
        let (manifest, sig, key) = signed(b"test installer", "0.20.0");
        let verified = verify_manifest(&manifest, &sig, &key, "v0.20.0", "0.19.0").unwrap();
        assert!(verify_installer(&mut Cursor::new(b"test installer"), &verified).is_ok());
        assert!(verify_installer(&mut Cursor::new(b"test installes"), &verified).is_err());
        assert!(verify_installer(&mut Cursor::new(b"short"), &verified).is_err());
        assert!(verify_manifest(&manifest, &sig, &key, "v0.21.0", "0.19.0").is_err());
        assert!(verify_manifest(&manifest, &sig, &key, "v0.20.0", "0.20.0").is_err());
        let mut bad = sig.clone();
        bad[0] ^= 1;
        assert!(verify_manifest(&manifest, &bad, &key, "v0.20.0", "0.19.0").is_err());
        let mut changed = manifest.clone();
        changed[32] ^= 1;
        assert!(verify_manifest(&changed, &sig, &key, "v0.20.0", "0.19.0").is_err());
        assert!(verify_manifest(&manifest, &sig, &[0; 32], "v0.20.0", "0.19.0").is_err());
    }

    #[test]
    fn rejects_noncanonical_versions() {
        for bad in ["v1.2.3", "1.2", "1.2.3.4", "1.02.3", "1.2.3-beta", "4294967296.0.0", "1.-1.0"] {
            assert!(version(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn verifies_metadata_produced_by_openssl_signing_script() {
        // A disposable test key, unrelated to the release key.
        fn decode(text: &str) -> Vec<u8> {
            (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
        }
        let key = decode("bb6a76f7b7434cba89eb70e346845fd73e69c0652891428f27b16e29423b0397");
        let sig = decode(concat!(
            "7d47d6a64eff19c25ee0d6b04fcd4114242f15cbc57822d254011584cc04f191",
            "acdf19d80de1afef51fa41002fb1280a050dff7ea7ac5801d7aa44c33131810c"
        ));
        let bytes =
            b"VelvetFrost update manifest v1\n0.20.0\nVelvetFrost-Setup.exe\n23\n478d3d0120522c0fc8b382d17b616f49088601fed3af873f8eb050283989b6b5\n";
        let manifest = verify_manifest(bytes, &sig, &key, "v0.20.0", "0.19.0").unwrap();
        verify_installer(&mut Cursor::new(b"isolated test installer"), &manifest).unwrap();
    }
}
