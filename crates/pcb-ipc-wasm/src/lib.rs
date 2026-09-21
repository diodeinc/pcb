//! In-memory IPC-2581 import, export, and DFM bindings.

mod options;

#[wasm_bindgen::prelude::wasm_bindgen(typescript_custom_section)]
const TYPESCRIPT: &str = include_str!("api.d.ts");

use pcb_ir::geom::Resolution;
use std::io::{Cursor, Read};

use anyhow::{Context, Result, bail};
use ipc2581::Ipc2581;
use pcb_ipc2581_tools::accessors::IpcAccessor;
use pcb_ipc2581_tools::commands::{self, dfm};
use pcb_ipc2581_tools::{UnitFormat, geometry, manufacturing, placement};
use pcb_ir::import::ipc2581::{ImportedDesign, import_design};
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::prelude::*;

use options::*;

const MAX_DECOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Warn).ok();
}

/// Parsed source with the imported geometry every export and DFM share.
///
/// The three representations answer different calls: the source text is what
/// `validate` and IPC-2581 export return, the typed model backs info, BOM and
/// outlines, and the imported design backs everything geometric. A file that
/// parses but cannot import, such as one without a Step, keeps the first two
/// and reports the import error from the calls that need geometry.
#[wasm_bindgen]
pub struct IpcDocument {
    xml: String,
    ipc: Ipc2581,
    input: dfm::report::FileIdentity,
    imported: Result<ImportedDesign, String>,
}

#[wasm_bindgen]
impl IpcDocument {
    #[wasm_bindgen(constructor)]
    pub fn new(
        xml: String,
        #[wasm_bindgen(unchecked_optional_param_type = "ImportOptions")] options: Option<JsValue>,
    ) -> Result<IpcDocument, JsError> {
        let ImportOptions { name, validate } = read_options(options)?;
        let input = input_identity(name, xml.as_bytes());
        Self::parse(xml, input, validate).map_err(js_error)
    }

    /// Accept UTF-8 XML or Zstandard, detected from bytes rather than the name.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(
        bytes: Vec<u8>,
        #[wasm_bindgen(unchecked_optional_param_type = "ImportOptions")] options: Option<JsValue>,
    ) -> Result<IpcDocument, JsError> {
        let ImportOptions { name, validate } = read_options(options)?;
        // The report identifies the bytes as given; decoding then consumes
        // them, so plain XML becomes the source text without a copy.
        let input = input_identity(name, &bytes);
        Self::parse(decode_xml(bytes).map_err(js_error)?, input, validate).map_err(js_error)
    }

    /// Validate the source against the bundled IPC-2581C schema.
    pub fn validate(&self) -> Result<(), JsError> {
        Ipc2581::validate(&self.xml).map_err(js_error)
    }

    /// The native IPC info JSON summary.
    #[wasm_bindgen(unchecked_return_type = "IpcInfo")]
    pub fn info(&self) -> Result<JsValue, JsError> {
        to_js(
            &self
                .design()
                .and_then(|design| commands::info::info_json(&self.accessor(), design))
                .map_err(js_error)?,
        )
    }

    /// Source layer names accepted by SVG and PNG export.
    pub fn layers(&self) -> Vec<String> {
        self.ipc.ecad().map_or_else(Vec::new, |ecad| {
            ecad.cad_data
                .layers
                .iter()
                .map(|layer| self.ipc.resolve(layer.name).to_owned())
                .collect()
        })
    }

    /// All formats return owned bytes, including multi-file Gerber/XNC output.
    #[wasm_bindgen(js_name = export, unchecked_return_type = "ExportFile[]")]
    pub fn export_files(
        &self,
        #[wasm_bindgen(unchecked_param_type = "ExportOptions")] options: JsValue,
    ) -> Result<JsValue, JsError> {
        to_js(
            &self
                .export_data(parse_options(options)?)
                .map_err(js_error)?,
        )
    }

    /// Violations return a report with verdict "fail". Invalid input throws.
    #[wasm_bindgen(js_name = checkDfm, unchecked_return_type = "DfmReport")]
    pub fn check_dfm(
        &self,
        #[wasm_bindgen(unchecked_optional_param_type = "DfmOptions")] options: Option<JsValue>,
    ) -> Result<JsValue, JsError> {
        let resolution = Resolution::default();

        let options: DfmOptions = read_options(options)?;
        let generated_at = match options.generated_at {
            Some(ref value) => chrono::DateTime::parse_from_rfc3339(value)
                .context("generatedAt must be an RFC 3339 timestamp")
                .map_err(js_error)?
                .with_timezone(&chrono::Utc),
            None => current_time().map_err(js_error)?,
        };
        let pdk = match &options.pdk {
            PdkInput::Builtin(name) => dfm::PdkSource::Builtin(name),
            PdkInput::Toml(input) => dfm::PdkSource::Toml(dfm::TextSource {
                path: input.name.as_deref().unwrap_or("pdk.toml"),
                source: &input.source,
            }),
        };
        to_js(
            &dfm::check(
                self.design().map_err(js_error)?,
                dfm::CheckRequest {
                    input: self.input.clone(),
                    pdk,
                    waivers: options.waivers.as_ref().map(|input| dfm::TextSource {
                        path: input.name.as_deref().unwrap_or("waivers.toml"),
                        source: &input.source,
                    }),
                    layout_target: options.layout_target,
                    generated_at,
                },
                resolution,
            )
            .map_err(js_error)?,
        )
    }
}

impl IpcDocument {
    fn parse(xml: String, input: dfm::report::FileIdentity, validate: bool) -> Result<Self> {
        let ipc = if validate {
            Ipc2581::parse_validated(&xml)
        } else {
            Ipc2581::parse(&xml)
        }
        .map_err(|error| match error {
            ipc2581::Ipc2581Error::SchemaValidation(_) => {
                anyhow::Error::new(error).context("IPC-2581 schema validation failed")
            }
            error => anyhow::Error::new(error).context("failed to parse IPC-2581 XML"),
        })?;
        let imported = import_design(&ipc, Resolution::default())
            .map_err(|error| format!("failed to import physical PCB design: {error:#}"));
        Ok(Self {
            xml,
            ipc,
            input,
            imported,
        })
    }

    fn accessor(&self) -> IpcAccessor<'_> {
        IpcAccessor::new(&self.ipc)
    }

    fn design(&self) -> Result<&ImportedDesign> {
        self.imported
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    fn export_data(&self, options: ExportOptions) -> Result<Vec<ExportFile>> {
        let resolution = Resolution::default();

        let file = match options {
            ExportOptions::Ipc2581 { mode } => ExportFile::new(
                "board.xml",
                "application/xml",
                match mode {
                    Some(mode) => commands::view::filter_by_mode(&self.xml, mode)?,
                    None => self.xml.clone(),
                },
            ),
            ExportOptions::Gerber { layout_target, zip } => {
                let package = manufacturing::build_manufacturing_package(
                    self.design()?,
                    &manufacturing::ManufacturingExportOptions {
                        view: layout_target.artwork_scope(),
                        relief_debug_dir: None,
                    },
                    resolution,
                )?;
                if zip {
                    ExportFile::new("manufacturing.zip", "application/zip", package.to_zip()?)
                } else {
                    return Ok(package
                        .files
                        .into_iter()
                        .map(|file| ExportFile::new(file.filename, "text/plain", file.contents))
                        .collect());
                }
            }
            ExportOptions::Svg {
                layer,
                layout_target,
            } => {
                let scope = layout_target.artwork_scope();
                let geometry =
                    geometry::render::prepare_layer(self.design()?, &layer, scope, resolution)?;
                ExportFile::new(
                    format!("{}.svg", safe_name(&layer)),
                    "image/svg+xml",
                    geometry::render::render_layer_svg(
                        &geometry,
                        true,
                        scope.profile_set(),
                        &pcb_ir::render::RenderOptions::default()
                            .with_accuracy(resolution.accuracy),
                    )?,
                )
            }
            ExportOptions::Png {
                layer,
                layout_target,
            } => {
                let scope = layout_target.artwork_scope();
                let geometry =
                    geometry::render::prepare_layer(self.design()?, &layer, scope, resolution)?;
                ExportFile::new(
                    format!("{}.png", safe_name(&layer)),
                    "image/png",
                    geometry::render::render_layer_png(
                        &geometry,
                        true,
                        scope.profile_set(),
                        resolution.accuracy,
                    )
                    .map_err(anyhow::Error::msg)?,
                )
            }
            ExportOptions::Dxf { layout_target } => ExportFile::new(
                "outline.dxf",
                "image/vnd.dxf",
                commands::outline::export_dxf(&self.ipc, layout_target, false, resolution)?,
            ),
            ExportOptions::Bom {} => ExportFile::new(
                "bom.json",
                "application/json",
                serde_json::to_vec_pretty(&commands::bom::extract_bom_lines(&self.accessor()))?,
            ),
            ExportOptions::Cpl { side, exclude_dnp } => {
                let placements = placement::extract_single_board_placements(self.design()?)?;
                ExportFile::new(
                    "placements.csv",
                    "text/csv",
                    commands::cpl::emit_cpl_csv(
                        &placements,
                        &commands::cpl::CplOptions {
                            output: None,
                            side,
                            exclude_dnp,
                        },
                    ),
                )
            }
            ExportOptions::Ict { side } => ExportFile::new(
                "ict.csv",
                "text/csv",
                commands::ict::emit_ict_csv(
                    &commands::ict::extract_contacts(&self.ipc, self.design()?, resolution)?,
                    side,
                ),
            ),
            ExportOptions::Html {} => ExportFile::new(
                "board.html",
                "text/html",
                commands::html_export::generate_html(
                    &self.accessor(),
                    self.design()?,
                    UnitFormat::Mm,
                    resolution,
                )?,
            ),
        };
        Ok(vec![file])
    }
}

fn input_identity(name: Option<String>, bytes: &[u8]) -> dfm::report::FileIdentity {
    dfm::report::FileIdentity::new(name.unwrap_or_else(|| "board.xml".into()), bytes)
}

#[wasm_bindgen(js_name = builtinPdks, unchecked_return_type = "BuiltinPdk[]")]
pub fn builtin_pdks() -> Result<JsValue, JsError> {
    to_js(dfm::builtin_pdks())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportFile {
    name: String,
    media_type: &'static str,
    #[serde(serialize_with = "serialize_bytes")]
    data: Vec<u8>,
}

impl ExportFile {
    fn new(name: impl Into<String>, media_type: &'static str, data: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            media_type,
            data: data.into(),
        }
    }
}

fn serialize_bytes<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bytes(bytes)
}

fn to_js<T: Serialize + ?Sized>(value: &T) -> Result<JsValue, JsError> {
    value
        .serialize(
            &serde_wasm_bindgen::Serializer::json_compatible().serialize_bytes_as_arrays(false),
        )
        .map_err(js_error)
}

fn read_options<T: DeserializeOwned + Default>(value: Option<JsValue>) -> Result<T, JsError> {
    match value {
        Some(value) if !value.is_null() && !value.is_undefined() => parse_options(value),
        _ => Ok(T::default()),
    }
}

fn parse_options<T: DeserializeOwned>(value: JsValue) -> Result<T, JsError> {
    // Direct struct deserialization skips unknown JS keys. Preserve every key
    // and number first, so strict serde validation sees the original input.
    let value: serde_value::Value = serde_wasm_bindgen::from_value(value).map_err(js_error)?;
    value.deserialize_into().map_err(js_error)
}

fn js_error(error: impl std::fmt::Display) -> JsError {
    JsError::new(&format!("{error:#}"))
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | ':') {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

fn decode_xml(bytes: Vec<u8>) -> Result<String> {
    if is_zstd_frame(&bytes) || is_skippable_frame(&bytes) {
        let bytes = bytes.as_slice();
        let mut input = Cursor::new(bytes);
        let mut decoded = Vec::new();
        while (input.position() as usize) < bytes.len() {
            let remaining = &bytes[input.position() as usize..];
            if is_skippable_frame(remaining) {
                let length = remaining
                    .get(4..8)
                    .context("truncated Zstandard skippable frame")?;
                let length = u32::from_le_bytes(length.try_into().unwrap()) as u64;
                let end = input.position() + 8 + length;
                if end > bytes.len() as u64 {
                    bail!("truncated Zstandard skippable frame");
                }
                input.set_position(end);
                continue;
            }
            if !is_zstd_frame(remaining) {
                bail!("invalid trailing data in Zstandard IPC input");
            }
            let frame_start = decoded.len();
            let mut decoder = ruzstd::decoding::StreamingDecoder::new(&mut input)
                .context("invalid Zstandard IPC input")?;
            // content_size() returns zero both for an absent size and an
            // explicitly empty frame. The header type is private in ruzstd;
            // bits 7-6 (size flag) or bit 5 (single segment) imply a size.
            // Successful decoder initialization guarantees this byte exists.
            let expected_size = (remaining[4] & 0xe0 != 0).then(|| decoder.decoder.content_size());
            (&mut decoder)
                .take(MAX_DECOMPRESSED_BYTES - decoded.len() as u64 + 1)
                .read_to_end(&mut decoded)
                .context("failed to decompress IPC input")?;
            if decoded.len() as u64 > MAX_DECOMPRESSED_BYTES {
                bail!("decompressed IPC input exceeds 256 MiB");
            }
            if let Some(expected) = expected_size
                && expected != (decoded.len() - frame_start) as u64
            {
                bail!("Zstandard IPC input content size mismatch");
            }
            // ruzstd reads the optional checksum but leaves verification to
            // its caller. Check each frame after collecting all its bytes.
            if let Some(expected) = decoder.decoder.get_checksum_from_data()
                && decoder.decoder.get_calculated_checksum() != Some(expected)
            {
                bail!("Zstandard IPC input checksum mismatch");
            }
        }
        String::from_utf8(decoded).context("IPC XML is not UTF-8")
    } else {
        String::from_utf8(bytes).context("IPC XML is not UTF-8")
    }
}

fn is_zstd_frame(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
}

fn is_skippable_frame(bytes: &[u8]) -> bool {
    matches!(bytes, [0x50..=0x5f, 0x2a, 0x4d, 0x18, ..])
}

#[cfg(target_arch = "wasm32")]
fn current_time() -> Result<chrono::DateTime<chrono::Utc>> {
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = Date, js_name = now)]
        fn now() -> f64;
    }
    chrono::DateTime::from_timestamp_millis(now() as i64)
        .context("host returned an invalid current time")
}

#[cfg(not(target_arch = "wasm32"))]
fn current_time() -> Result<chrono::DateTime<chrono::Utc>> {
    Ok(chrono::Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(xml: &str) -> IpcDocument {
        IpcDocument::parse(xml.to_owned(), input_identity(None, xml.as_bytes()), false).unwrap()
    }

    #[test]
    fn a_document_without_geometry_still_answers_what_needs_none() {
        let document = document(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="BOM"/>
  </Content>
</IPC-2581>"#,
        );

        let error = document.design().unwrap_err().to_string();
        assert!(
            error.contains("failed to import physical PCB design"),
            "{error}"
        );
        assert!(document.export_data(ExportOptions::Bom {}).is_ok());
        assert!(
            document
                .export_data(ExportOptions::Ipc2581 { mode: None })
                .is_ok()
        );
        assert!(document.export_data(ExportOptions::Html {}).is_err());
    }

    #[test]
    fn plain_bytes_decode_without_a_copy_and_keep_their_identity() {
        let xml = include_str!("../tests/board.xml");
        let input = input_identity(Some("fixture.xml".into()), xml.as_bytes());
        assert_eq!(input.size_bytes, xml.len() as u64);

        let bytes = xml.as_bytes().to_vec();
        let pointer = bytes.as_ptr();
        let decoded = decode_xml(bytes).unwrap();
        assert_eq!(decoded.as_ptr(), pointer);

        let document = IpcDocument::parse(decoded, input, true).unwrap();
        assert!(document.design().is_ok());
    }
}
