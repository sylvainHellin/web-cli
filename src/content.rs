//! Classify a fetched body as text or binary from its Content-Type header and
//! its leading bytes, so binary payloads (PDF, Office, images) are saved
//! verbatim instead of being decoded into lossy text.

/// A fetched response body, after classification.
#[derive(Debug)]
pub enum Body {
    Text(String),
    Binary {
        bytes: Vec<u8>,
        content_type: Option<String>,
        ext: &'static str,
    },
}

/// File signatures recognised regardless of the Content-Type header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Magic {
    Pdf,
    Zip,
    Png,
    Jpg,
    Gif,
}

fn sniff_magic(bytes: &[u8]) -> Option<Magic> {
    if bytes.starts_with(b"%PDF-") {
        Some(Magic::Pdf)
    } else if bytes.starts_with(b"PK\x03\x04") {
        Some(Magic::Zip)
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Magic::Png)
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some(Magic::Jpg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Magic::Gif)
    } else {
        None
    }
}

/// Split a Content-Type header into its lowercased media type and its
/// `charset` parameter, if any.
fn parse_content_type(content_type: &str) -> (String, Option<String>) {
    let mut parts = content_type.split(';');
    let media = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let charset = parts.find_map(|p| {
        let (k, v) = p.split_once('=')?;
        if k.trim().eq_ignore_ascii_case("charset") {
            Some(v.trim().trim_matches('"').to_string())
        } else {
            None
        }
    });
    (media, charset)
}

fn is_text_media(media: &str) -> bool {
    media.starts_with("text/")
        || matches!(
            media,
            "application/json"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/javascript"
        )
        || media.ends_with("+json")
        || media.ends_with("+xml")
}

fn is_binary_media(media: &str) -> bool {
    media.starts_with("image/")
        || media.starts_with("audio/")
        || media.starts_with("video/")
        || media.starts_with("application/vnd.openxmlformats-")
        || media.starts_with("application/vnd.ms-")
        || matches!(
            media,
            "application/pdf" | "application/zip" | "application/msword"
        )
}

/// Pick a file extension for a binary body.
fn extension(media: &str, magic: Option<Magic>) -> &'static str {
    match magic {
        Some(Magic::Png) => return "png",
        Some(Magic::Jpg) => return "jpg",
        Some(Magic::Gif) => return "gif",
        Some(Magic::Pdf) => return "pdf",
        Some(Magic::Zip) | None => {}
    }
    if media == "application/pdf" && magic.is_none() {
        return "pdf";
    }
    if media.contains("wordprocessingml") {
        "docx"
    } else if media.contains("spreadsheetml") {
        "xlsx"
    } else if media.contains("presentationml") {
        "pptx"
    } else {
        "bin"
    }
}

/// Decode text bytes per the declared charset, the way reqwest's
/// `Response::text()` does: unknown or missing charset falls back to UTF-8,
/// a BOM wins, and invalid sequences become U+FFFD.
fn decode(bytes: &[u8], charset: Option<&str>) -> String {
    let encoding = charset
        .and_then(|c| encoding_rs::Encoding::for_label(c.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// Classify a body as text or binary. Magic bytes win over the header, since
/// servers often send PDFs as octet-stream or with a wrong type.
pub fn classify(content_type: Option<&str>, bytes: Vec<u8>) -> Body {
    let (media, charset) = content_type.map(parse_content_type).unwrap_or_default();
    let magic = sniff_magic(&bytes);
    let binary = |bytes: Vec<u8>| Body::Binary {
        ext: extension(&media, magic),
        content_type: content_type.map(str::to_string),
        bytes,
    };

    if magic.is_some() {
        return binary(bytes);
    }
    if bytes.is_empty() {
        return Body::Text(String::new());
    }
    if is_text_media(&media) {
        return Body::Text(decode(&bytes, charset.as_deref()));
    }
    if is_binary_media(&media) {
        return binary(bytes);
    }
    // Header missing, octet-stream or unknown: text only if it is clean UTF-8
    // with no NUL near the start. Never decode lossily here.
    let head = &bytes[..bytes.len().min(8 * 1024)];
    if head.contains(&0) {
        return binary(bytes);
    }
    match String::from_utf8(bytes) {
        Ok(text) => Body::Text(text),
        Err(e) => binary(e.into_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PDF: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj";
    const ZIP: &[u8] = b"PK\x03\x04\x14\x00\x06\x00rest";

    fn ext_of(body: Body) -> &'static str {
        match body {
            Body::Binary { ext, .. } => ext,
            Body::Text(t) => panic!("expected binary, got text: {t:?}"),
        }
    }

    fn text_of(body: Body) -> String {
        match body {
            Body::Text(t) => t,
            Body::Binary { ext, .. } => panic!("expected text, got binary .{ext}"),
        }
    }

    #[test]
    fn pdf_magic_wins_over_any_header() {
        for ct in [
            Some("application/pdf"),
            Some("application/octet-stream"),
            None,
            Some("text/html; charset=utf-8"),
        ] {
            assert_eq!(ext_of(classify(ct, PDF.to_vec())), "pdf", "header {ct:?}");
        }
    }

    #[test]
    fn binary_keeps_bytes_and_header() {
        match classify(Some("application/pdf"), PDF.to_vec()) {
            Body::Binary {
                bytes,
                content_type,
                ext,
            } => {
                assert_eq!(bytes, PDF);
                assert_eq!(content_type.as_deref(), Some("application/pdf"));
                assert_eq!(ext, "pdf");
            }
            Body::Text(_) => panic!("expected binary"),
        }
    }

    #[test]
    fn zip_family_extensions() {
        let cases = [
            (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "docx",
            ),
            (
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "xlsx",
            ),
            (
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                "pptx",
            ),
            ("application/octet-stream", "bin"),
        ];
        for (ct, ext) in cases {
            assert_eq!(ext_of(classify(Some(ct), ZIP.to_vec())), ext, "header {ct}");
        }
    }

    #[test]
    fn image_magic_extensions() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
        let jpg = b"\xFF\xD8\xFF\xE0\x00\x10JFIF".to_vec();
        let gif7 = b"GIF87a\x01\x00".to_vec();
        let gif9 = b"GIF89a\x01\x00".to_vec();
        assert_eq!(ext_of(classify(None, png)), "png");
        assert_eq!(
            ext_of(classify(Some("application/octet-stream"), jpg)),
            "jpg"
        );
        assert_eq!(ext_of(classify(Some("image/gif"), gif7)), "gif");
        assert_eq!(ext_of(classify(None, gif9)), "gif");
    }

    #[test]
    fn text_headers_decode_as_text() {
        let cases = [
            "text/html; charset=utf-8",
            "application/json",
            "application/ld+json",
            "image/svg+xml",
            "Text/HTML",
        ];
        for ct in cases {
            assert_eq!(
                text_of(classify(Some(ct), b"<p>hi</p>".to_vec())),
                "<p>hi</p>"
            );
        }
    }

    #[test]
    fn no_header_valid_utf8_is_text() {
        let body = "Grüße, plain text".as_bytes().to_vec();
        assert_eq!(text_of(classify(None, body)), "Grüße, plain text");
    }

    #[test]
    fn no_header_invalid_utf8_is_binary() {
        assert_eq!(ext_of(classify(None, b"abc\xFF\xFEdef".to_vec())), "bin");
    }

    #[test]
    fn no_header_nul_is_binary() {
        assert_eq!(ext_of(classify(None, b"abc\x00def".to_vec())), "bin");
        assert_eq!(
            ext_of(classify(
                Some("application/octet-stream"),
                b"abc\x00def".to_vec()
            )),
            "bin"
        );
    }

    #[test]
    fn binary_header_without_magic() {
        assert_eq!(
            ext_of(classify(Some("image/webp"), b"RIFF\x00\x00WEBP".to_vec())),
            "bin"
        );
        assert_eq!(
            ext_of(classify(Some("application/pdf"), b"not really".to_vec())),
            "pdf"
        );
    }

    #[test]
    fn latin1_charset_is_decoded() {
        let body = b"<p>Gr\xFC\xDFe</p>".to_vec();
        let text = text_of(classify(Some("text/html; charset=iso-8859-1"), body));
        assert_eq!(text, "<p>Grüße</p>");
    }

    #[test]
    fn empty_body_is_text() {
        assert_eq!(text_of(classify(None, Vec::new())), "");
        assert_eq!(text_of(classify(Some("application/pdf"), Vec::new())), "");
    }
}
