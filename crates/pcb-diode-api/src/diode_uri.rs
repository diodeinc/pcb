use std::str::FromStr;

use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiodeUri {
    SandboxFile(SandboxFileUri),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxFileUri {
    pub host: String,
    pub sandbox_id: String,
    pub sandbox_path: String,
}

impl SandboxFileUri {
    pub fn api_base_url(&self) -> String {
        host_api_base_url(&self.host)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DiodeUriParseError {
    #[error("expected a diode:// URI")]
    UnsupportedScheme,
    #[error("invalid URI: {0}")]
    InvalidUri(String),
    #[error("diode URI must include a host")]
    MissingHost,
    #[error("diode URI must not include user info")]
    UserInfoNotSupported,
    #[error("diode URI must not include a query string")]
    QueryNotSupported,
    #[error("diode URI must not include a fragment")]
    FragmentNotSupported,
    #[error("expected /sandboxes/{{sandboxId}}/fs/{{absolutePath}}")]
    UnsupportedPath,
    #[error("sandbox id must not be empty")]
    EmptySandboxId,
    #[error("sandbox filesystem path must not be empty")]
    EmptySandboxPath,
    #[error("sandbox filesystem path must not include empty, '.', or '..' segments")]
    UnsafeSandboxPath,
    #[error("sandbox filesystem path segment must not decode to a path separator")]
    PathSeparatorInSegment,
    #[error("invalid percent-encoding in URI path")]
    InvalidPathEncoding,
}

impl DiodeUri {
    pub fn parse(input: &str) -> Result<Self, DiodeUriParseError> {
        input.parse()
    }
}

impl SandboxFileUri {
    pub fn parse(input: &str) -> Result<Self, DiodeUriParseError> {
        match DiodeUri::parse(input)? {
            DiodeUri::SandboxFile(uri) => Ok(uri),
        }
    }
}

impl FromStr for DiodeUri {
    type Err = DiodeUriParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if !is_diode_uri(input) {
            return Err(DiodeUriParseError::UnsupportedScheme);
        }

        let url =
            Url::parse(input).map_err(|err| DiodeUriParseError::InvalidUri(err.to_string()))?;
        if url.scheme() != "diode" {
            return Err(DiodeUriParseError::UnsupportedScheme);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(DiodeUriParseError::UserInfoNotSupported);
        }
        if url.query().is_some() {
            return Err(DiodeUriParseError::QueryNotSupported);
        }
        if url.fragment().is_some() {
            return Err(DiodeUriParseError::FragmentNotSupported);
        }

        let host = url.host_str().ok_or(DiodeUriParseError::MissingHost)?;
        let host = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };

        let raw_path = raw_path(input).ok_or(DiodeUriParseError::UnsupportedPath)?;
        let raw_segments: Vec<&str> = raw_path
            .strip_prefix('/')
            .ok_or(DiodeUriParseError::UnsupportedPath)?
            .split('/')
            .collect();
        if raw_segments.len() < 3 || raw_segments[0] != "sandboxes" || raw_segments[2] != "fs" {
            return Err(DiodeUriParseError::UnsupportedPath);
        }

        let sandbox_id = decode_segment(raw_segments[1])?;
        if sandbox_id.is_empty() {
            return Err(DiodeUriParseError::EmptySandboxId);
        }

        let sandbox_path_segments = &raw_segments[3..];
        if sandbox_path_segments.is_empty() {
            return Err(DiodeUriParseError::EmptySandboxPath);
        }

        let mut decoded_path_segments = Vec::with_capacity(sandbox_path_segments.len());
        for segment in sandbox_path_segments {
            let segment = decode_segment(segment)?;
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(DiodeUriParseError::UnsafeSandboxPath);
            }
            decoded_path_segments.push(segment);
        }

        Ok(DiodeUri::SandboxFile(SandboxFileUri {
            host,
            sandbox_id,
            sandbox_path: format!("/{}", decoded_path_segments.join("/")),
        }))
    }
}

pub fn is_diode_uri(input: &str) -> bool {
    input
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("diode://"))
}

fn raw_path(input: &str) -> Option<&str> {
    let after_scheme = input.split_once("://")?.1;
    let path_start = after_scheme.find('/')?;
    let rest = &after_scheme[path_start..];
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    Some(&rest[..path_end])
}

fn decode_segment(segment: &str) -> Result<String, DiodeUriParseError> {
    let decoded = urlencoding::decode(segment)
        .map_err(|_| DiodeUriParseError::InvalidPathEncoding)?
        .into_owned();
    if decoded.contains('/') || decoded.contains('\\') {
        return Err(DiodeUriParseError::PathSeparatorInSegment);
    }
    Ok(decoded)
}

fn host_api_base_url(host: &str) -> String {
    let scheme = if host.starts_with("localhost") || host.starts_with("127.") {
        "http"
    } else {
        "https"
    };
    format!("{scheme}://{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sandbox_file_uri() {
        let uri = SandboxFileUri::parse(
            "diode://registry.diode.computer/sandboxes/sbx_123/fs/home/sandbox/registry/boards/foo/main.zen",
        )
        .unwrap();

        assert_eq!(uri.host, "registry.diode.computer");
        assert_eq!(uri.api_base_url(), "https://registry.diode.computer");
        assert_eq!(uri.sandbox_id, "sbx_123");
        assert_eq!(
            uri.sandbox_path,
            "/home/sandbox/registry/boards/foo/main.zen"
        );
    }

    #[test]
    fn parses_host_with_port_and_percent_decoded_path() {
        let uri = SandboxFileUri::parse(
            "diode://localhost:3001/sandboxes/sbx_123/fs/home/sandbox/registry/boards/My%20Board/main.zen",
        )
        .unwrap();

        assert_eq!(uri.host, "localhost:3001");
        assert_eq!(uri.api_base_url(), "http://localhost:3001");
        assert_eq!(
            uri.sandbox_path,
            "/home/sandbox/registry/boards/My Board/main.zen"
        );
    }

    #[test]
    fn rejects_wrong_scheme() {
        assert_eq!(
            DiodeUri::parse(
                "https://registry.diode.computer/sandboxes/sbx_123/fs/home/sandbox/main.zen"
            )
            .unwrap_err(),
            DiodeUriParseError::UnsupportedScheme
        );
    }

    #[test]
    fn rejects_unsupported_path_shape() {
        assert_eq!(
            DiodeUri::parse("diode://registry.diode.computer/repositories/repo_123/main.zen")
                .unwrap_err(),
            DiodeUriParseError::UnsupportedPath
        );
    }

    #[test]
    fn rejects_empty_sandbox_path() {
        assert_eq!(
            DiodeUri::parse("diode://registry.diode.computer/sandboxes/sbx_123/fs").unwrap_err(),
            DiodeUriParseError::EmptySandboxPath
        );
    }

    #[test]
    fn rejects_path_traversal_segments() {
        assert_eq!(
            DiodeUri::parse(
                "diode://registry.diode.computer/sandboxes/sbx_123/fs/home/sandbox/../main.zen"
            )
            .unwrap_err(),
            DiodeUriParseError::UnsafeSandboxPath
        );
    }

    #[test]
    fn rejects_encoded_path_separators() {
        assert_eq!(
            DiodeUri::parse(
                "diode://registry.diode.computer/sandboxes/sbx_123/fs/home/sandbox%2Fregistry/main.zen"
            )
            .unwrap_err(),
            DiodeUriParseError::PathSeparatorInSegment
        );
    }
}
