//! In-memory IPC-2581 import, export, and DFM bindings.

mod options;

#[wasm_bindgen::prelude::wasm_bindgen(typescript_custom_section)]
const TYPESCRIPT: &str = include_str!("api.d.ts");

use std::cell::OnceCell;
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

/// Parsed source with lazily cached geometry shared by exports and DFM.
#[wasm_bindgen]
pub struct IpcDocument {
    xml: String,
    ipc: Ipc2581,
    input: dfm::report::FileIdentity,
    imported: OnceCell<ImportedDesign>,
}

#[wasm_bindgen]
impl IpcDocument {
    #[wasm_bindgen(constructor)]
    pub fn new(
        xml: &str,
        #[wasm_bindgen(unchecked_optional_param_type = "ImportOptions")] options: Option<JsValue>,
    ) -> Result<IpcDocument, JsError> {
        Self::parse(xml.to_owned(), xml.as_bytes(), read_options(options)?).map_err(js_error)
    }

    /// Accept UTF-8 XML or Zstandard, detected from bytes rather than the name.
    #[wasm_bindgen(js_name = fromBytes)]
    pub fn from_bytes(
        bytes: &[u8],
        #[wasm_bindgen(unchecked_optional_param_type = "ImportOptions")] options: Option<JsValue>,
    ) -> Result<IpcDocument, JsError> {
        Self::parse(
            decode_xml(bytes).map_err(js_error)?,
            bytes,
            read_options(options)?,
        )
        .map_err(js_error)
    }

    /// Validate the source against the bundled IPC-2581C schema.
    pub fn validate(&self) -> Result<(), JsError> {
        Ipc2581::validate(&self.xml).map_err(js_error)
    }

    /// The native IPC info JSON summary.
    #[wasm_bindgen(unchecked_return_type = "IpcInfo")]
    pub fn info(&self) -> Result<JsValue, JsError> {
        to_js(&commands::info::info_json(&self.accessor()))
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
            )
            .map_err(js_error)?,
        )
    }
}

impl IpcDocument {
    fn parse(xml: String, original: &[u8], options: ImportOptions) -> Result<Self> {
        if options.validate {
            Ipc2581::validate(&xml).context("IPC-2581 schema validation failed")?;
        }
        Ok(Self {
            ipc: Ipc2581::parse(&xml).context("failed to parse IPC-2581 XML")?,
            input: dfm::report::FileIdentity::new(
                options.name.unwrap_or_else(|| "board.xml".into()),
                original,
            ),
            xml,
            imported: OnceCell::new(),
        })
    }

    fn accessor(&self) -> IpcAccessor<'_> {
        IpcAccessor::new(&self.ipc)
    }

    fn design(&self) -> Result<&ImportedDesign> {
        if self.imported.get().is_none() {
            let imported =
                import_design(&self.ipc).context("failed to import physical PCB design")?;
            let _ = self.imported.set(imported);
        }
        Ok(self.imported.get().expect("design was initialized"))
    }

    fn export_data(&self, options: ExportOptions) -> Result<Vec<ExportFile>> {
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
                let package = manufacturing::build_manufacturing_package_from_design(
                    self.design()?,
                    layout_target.artwork_scope(),
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
                let geometry = geometry::render::prepare_layer(self.design()?, &layer, scope)?;
                ExportFile::new(
                    format!("{}.svg", safe_name(&layer)),
                    "image/svg+xml",
                    geometry::render::render_layer_svg(&geometry, true, scope.profile_set()),
                )
            }
            ExportOptions::Png {
                layer,
                layout_target,
            } => {
                let scope = layout_target.artwork_scope();
                let geometry = geometry::render::prepare_layer(self.design()?, &layer, scope)?;
                ExportFile::new(
                    format!("{}.png", safe_name(&layer)),
                    "image/png",
                    geometry::render::render_layer_png(&geometry, true, scope.profile_set())
                        .map_err(anyhow::Error::msg)?,
                )
            }
            ExportOptions::Dxf { layout_target } => ExportFile::new(
                "outline.dxf",
                "image/vnd.dxf",
                commands::outline::export_dxf(&self.ipc, layout_target, false)?,
            ),
            ExportOptions::Bom {} => ExportFile::new(
                "bom.json",
                "application/json",
                serde_json::to_vec_pretty(&commands::bom::extract_bom_lines(&self.accessor()))?,
            ),
            ExportOptions::Cpl { side, exclude_dnp } => {
                let placements = placement::extract_single_board_placements_from_design(
                    &self.accessor(),
                    self.design()?,
                )?;
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
                    &commands::ict::extract_contacts_from_design(&self.ipc, self.design()?)?,
                    side,
                ),
            ),
            ExportOptions::Html {} => ExportFile::new(
                "board.html",
                "text/html",
                commands::html_export::generate_html(&self.accessor(), UnitFormat::Mm)?,
            ),
        };
        Ok(vec![file])
    }
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

fn decode_xml(bytes: &[u8]) -> Result<String> {
    if is_zstd_frame(bytes) || is_skippable_frame(bytes) {
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
        String::from_utf8(bytes.to_vec()).context("IPC XML is not UTF-8")
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
