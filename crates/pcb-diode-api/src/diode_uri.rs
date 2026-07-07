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

    pub fn to_read_uri_string(&self) -> String {
        format!(
            "diode://{}/sandboxes/{}/fs/read?path={}",
            self.host,
            urlencoding::encode(&self.sandbox_id),
            urlencoding::encode(&self.sandbox_path)
        )
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
    #[error(
        "diode URI query string is only supported for /sandboxes/{{sandboxId}}/fs/read?path={{absolutePath}}"
    )]
    QueryNotSupported,
    #[error("diode URI must not include a fragment")]
    FragmentNotSupported,
    #[error("expected /sandboxes/{{sandboxId}}/fs/read?path={{absolutePath}}")]
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

        if url.query().is_none() {
            return Err(DiodeUriParseError::UnsupportedPath);
        }
        let sandbox_path = parse_query_sandbox_path(&url, &raw_segments)?;

        Ok(DiodeUri::SandboxFile(SandboxFileUri {
            host,
            sandbox_id,
            sandbox_path,
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

fn parse_query_sandbox_path(
    url: &Url,
    raw_segments: &[&str],
) -> Result<String, DiodeUriParseError> {
    if raw_segments.len() != 4 || raw_segments[3] != "read" {
        return Err(DiodeUriParseError::QueryNotSupported);
    }
    if url.query().is_none() {
        return Err(DiodeUriParseError::QueryNotSupported);
    }

    let mut sandbox_path = None;
    for (key, value) in url.query_pairs() {
        if key != "path" || sandbox_path.is_some() {
            return Err(DiodeUriParseError::QueryNotSupported);
        }
        sandbox_path = Some(value.into_owned());
    }
    let sandbox_path = sandbox_path.ok_or(DiodeUriParseError::EmptySandboxPath)?;
    validate_absolute_sandbox_path(&sandbox_path)?;
    Ok(sandbox_path)
}

fn validate_absolute_sandbox_path(path: &str) -> Result<(), DiodeUriParseError> {
    if path.is_empty() {
        return Err(DiodeUriParseError::EmptySandboxPath);
    }
    if !path.starts_with('/') {
        return Err(DiodeUriParseError::UnsupportedPath);
    }
    for segment in path.split('/').skip(1) {
        if !is_safe_path_segment(segment) {
            return Err(DiodeUriParseError::UnsafeSandboxPath);
        }
    }
    Ok(())
}

fn is_safe_path_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".." && !segment.contains('\\')
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
    fn parses_sandbox_file_uri_with_query_path() {
        let uri = SandboxFileUri::parse(
            "diode://api.diode.computer/sandboxes/pcb-ai-a49caa18-eae9-48d7-8b17-72d0de6cf7d6/fs/read?path=%2Fhome%2Fsandbox%2Fregistry%2Fcomponents%2FTexas_Instruments%2FTUSB321%2Flayout%2FPortController%2Flayout.kicad_pcb",
        )
        .unwrap();

        assert_eq!(uri.host, "api.diode.computer");
        assert_eq!(uri.api_base_url(), "https://api.diode.computer");
        assert_eq!(
            uri.sandbox_id,
            "pcb-ai-a49caa18-eae9-48d7-8b17-72d0de6cf7d6"
        );
        assert_eq!(
            uri.sandbox_path,
            "/home/sandbox/registry/components/Texas_Instruments/TUSB321/layout/PortController/layout.kicad_pcb"
        );
    }

    #[test]
    fn parses_host_with_port_and_query_path() {
        let uri = SandboxFileUri::parse(
            "diode://localhost:3001/sandboxes/sbx_123/fs/read?path=%2Fhome%2Fsandbox%2Fregistry%2Fboards%2FMy%20Board%2Fmain.zen",
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
    fn formats_query_style_sandbox_file_uri() {
        let uri = SandboxFileUri {
            host: "api.diode.computer".to_string(),
            sandbox_id: "sandbox/id".to_string(),
            sandbox_path: "/home/sandbox/registry/components/LT3010EMS8E#PBF/LT3010x.zen"
                .to_string(),
        };

        assert_eq!(
            uri.to_read_uri_string(),
            "diode://api.diode.computer/sandboxes/sandbox%2Fid/fs/read?path=%2Fhome%2Fsandbox%2Fregistry%2Fcomponents%2FLT3010EMS8E%23PBF%2FLT3010x.zen"
        );
    }

    #[test]
    fn rejects_query_path_without_absolute_path() {
        assert_eq!(
            DiodeUri::parse("diode://api.diode.computer/sandboxes/sbx_123/fs/read?path=relative")
                .unwrap_err(),
            DiodeUriParseError::UnsupportedPath
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
    fn rejects_missing_query_path() {
        assert_eq!(
            DiodeUri::parse("diode://registry.diode.computer/sandboxes/sbx_123/fs/read?foo=bar")
                .unwrap_err(),
            DiodeUriParseError::QueryNotSupported
        );
    }

    #[test]
    fn rejects_query_path_traversal_segments() {
        assert_eq!(
            DiodeUri::parse(
                "diode://registry.diode.computer/sandboxes/sbx_123/fs/read?path=%2Fhome%2Fsandbox%2F..%2Fmain.zen"
            )
            .unwrap_err(),
            DiodeUriParseError::UnsafeSandboxPath
        );
    }

    #[test]
    fn rejects_encoded_path_separators() {
        assert_eq!(
            DiodeUri::parse(
                "diode://registry.diode.computer/sandboxes/sbx%2F123/fs/read?path=%2Fhome%2Fsandbox%2Fregistry%2Fmain.zen"
            )
            .unwrap_err(),
            DiodeUriParseError::PathSeparatorInSegment
        );
    }
}
