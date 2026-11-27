use anyhow::Result;
use reqwest::blocking::RequestBuilder;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
use serde::Serialize;

/// JSON request bodies compressed with zstd, which the Diode API decodes.
pub(crate) trait JsonZstd: Sized {
    fn json_zstd<T: Serialize + ?Sized>(self, body: &T) -> Result<Self>;
}

impl JsonZstd for RequestBuilder {
    fn json_zstd<T: Serialize + ?Sized>(self, body: &T) -> Result<Self> {
        let mut encoder = zstd::Encoder::new(Vec::new(), 0)?;
        serde_json::to_writer(&mut encoder, body)?;
        Ok(self
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_ENCODING, "zstd")
            .body(encoder.finish()?))
    }
}
