use crate::{Ipc2581Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::{Digest, Md5};

/// Splits off the base64 MD5 digest that IPC-2581 allows after the root
/// element's closing tag, returning the XML document and the digest.
///
/// Any other text after the last tag stays in the document for the XML
/// parser to judge.
pub fn split_trailer(xml: &str) -> (&str, Option<[u8; 16]>) {
    let end = xml.rfind('>').map_or(0, |index| index + 1);
    let digest = STANDARD
        .decode(xml[end..].trim())
        .ok()
        .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok());
    match digest {
        Some(digest) => (&xml[..end], Some(digest)),
        None => (xml, None),
    }
}

/// Checks `digest` against the MD5 of the root element's source text, from
/// `<IPC-2581` through the closing tag inclusive.
pub fn verify(root_source: &str, digest: [u8; 16]) -> Result<()> {
    let actual = Md5::digest(root_source.as_bytes());
    if actual[..] == digest {
        Ok(())
    } else {
        Err(Ipc2581Error::ChecksumMismatch {
            expected: hex::encode(digest),
            actual: hex::encode(actual),
        })
    }
}

/// Parses the XML document in `xml`, verifying its checksum trailer if any.
pub fn parse_document(xml: &str) -> Result<uppsala::Document<'_>> {
    let (xml, digest) = split_trailer(xml);
    let doc = uppsala::parse(xml).map_err(|err| Ipc2581Error::XmlParse(err.to_string()))?;
    if let (Some(digest), Some(root)) = (digest, doc.document_element()) {
        let range = doc.node_range(root).expect("root comes from parsed source");
        verify(&xml[range], digest)?;
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "<IPC-2581 revision=\"C\">\n  <Content roleRef=\"Owner\"/>\n</IPC-2581>";

    fn with_trailer(root: &str, digested: &str) -> String {
        let digest = STANDARD.encode(Md5::digest(digested.as_bytes()));
        format!("<?xml version=\"1.0\"?>\n{root}\n{digest}\n")
    }

    #[test]
    fn document_without_trailer_is_untouched() {
        let xml = format!("<?xml version=\"1.0\"?>\n{ROOT}\n<!-- a > b -->\n");
        assert_eq!(split_trailer(&xml), (xml.as_str(), None));
        assert!(parse_document(&xml).is_ok());
    }

    #[test]
    fn valid_trailer_is_stripped_and_verified() {
        let xml = with_trailer(ROOT, ROOT);
        let (body, digest) = split_trailer(&xml);
        assert!(body.ends_with("</IPC-2581>"));
        assert!(digest.is_some());
        assert!(parse_document(&xml).is_ok());
    }

    #[test]
    fn mismatched_trailer_is_rejected() {
        let error = parse_document(&with_trailer(ROOT, "something else")).unwrap_err();
        assert!(matches!(error, Ipc2581Error::ChecksumMismatch { .. }));
    }

    #[test]
    fn root_is_located_by_the_xml_parser() {
        // A commented-out close tag and digest ahead of the root, a prefixed
        // root, and whitespace inside the close tag.
        let root = "<ipc:IPC-2581 xmlns:ipc=\"http://webstds.ipc.org/2581\"/>";
        for root in [ROOT, root, &ROOT.replace("</IPC-2581>", "</IPC-2581 >")] {
            let xml = with_trailer(
                &format!("<!--</IPC-2581>\nAAAAAAAAAAAAAAAAAAAAAA==\n<IPC-2581-->{root}"),
                root,
            );
            assert!(parse_document(&xml).is_ok(), "{xml}");
        }
    }

    #[test]
    fn non_digest_trailer_is_left_for_the_xml_parser() {
        let xml = format!("{ROOT}\nnot a digest\n");
        assert_eq!(split_trailer(&xml), (xml.as_str(), None));
        assert!(matches!(
            parse_document(&xml),
            Err(Ipc2581Error::XmlParse(_))
        ));
    }
}
