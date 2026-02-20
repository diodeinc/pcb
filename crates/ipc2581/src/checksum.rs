use crate::{Ipc2581Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::{Digest, Md5};

/// Validates MD5 checksum if present in IPC-2581 file
/// The checksum follows the closing </IPC-2581> tag and is base64 encoded
pub fn validate_checksum(xml: &str) -> Result<()> {
    // Find the closing tag
    let closing_tag = "</IPC-2581>";
    let Some(end_pos) = xml.find(closing_tag) else {
        return Err(Ipc2581Error::InvalidStructure(
            "Missing closing </IPC-2581> tag".to_string(),
        ));
    };

    let tag_end = end_pos + closing_tag.len();

    // Check if there's a checksum after the closing tag
    let after_tag = &xml[tag_end..].trim();
    if after_tag.is_empty() {
        // No checksum present, that's OK
        return Ok(());
    }

    // Extract the base64 checksum (should be on the next line)
    let checksum_line = after_tag.lines().next().unwrap_or("").trim();
    if checksum_line.is_empty() {
        // No checksum
        return Ok(());
    }

    // Decode base64 checksum
    let expected_bytes = match STANDARD.decode(checksum_line) {
        Ok(bytes) => bytes,
        Err(_) => {
            // Not a valid base64 string, might be something else
            return Ok(());
        }
    };

    if expected_bytes.len() != 16 {
        // MD5 is always 16 bytes
        return Ok(());
    }

    // Compute MD5 of content from <IPC-2581> to </IPC-2581> inclusive
    let Some(start_pos) = xml.find("<IPC-2581") else {
        return Err(Ipc2581Error::InvalidStructure(
            "Missing opening <IPC-2581> tag".to_string(),
        ));
    };

    let content = &xml[start_pos..tag_end];
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    let actual_bytes = hasher.finalize();

    if expected_bytes[..] != actual_bytes[..] {
        return Err(Ipc2581Error::ChecksumMismatch {
            expected: format!("{:x}", md5::Md5::digest(expected_bytes)),
            actual: format!("{:x}", md5::Md5::digest(actual_bytes)),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_checksum() {
        let xml = r#"<?xml version="1.0"?>
<IPC-2581 revision="C">
  <Content roleRef="Owner"/>
</IPC-2581>"#;
        assert!(validate_checksum(xml).is_ok());
    }

    #[test]
    fn test_valid_checksum() {
        let content = r#"<IPC-2581 revision="C">
  <Content roleRef="Owner"/>
</IPC-2581>"#;

        // Compute the actual MD5
        let mut hasher = Md5::new();
        hasher.update(content.as_bytes());
        let digest = hasher.finalize();

        // Base64 encode it
        let b64 = STANDARD.encode(digest);

        let xml = format!("<?xml version=\"1.0\"?>\n{}\n{}", content, b64);

        assert!(validate_checksum(&xml).is_ok());
    }
}
