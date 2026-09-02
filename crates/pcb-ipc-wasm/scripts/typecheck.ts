// Compile against the generated package. smoke.mjs covers actual execution.
import init, {
  IpcDocument,
  builtinPdks,
  type DfmReport,
  type ExportFile,
  type ExportOptions,
} from "../../../target/ipc-wasm-bundle/pcb_ipc_wasm.js";

function expectType<T>(_value: T): void {}

export async function checkUsage(
  module: WebAssembly.Module,
  xml: string,
  bytes: Uint8Array,
  pdkToml: string,
  waiverToml: string,
): Promise<void> {
  await init({ module_or_path: module });
  const pcb = new IpcDocument(xml, { name: "board.xml", validate: true });
  const decoded = IpcDocument.fromBytes(bytes, { name: "board.xml.zst" });
  try {
    pcb.validate();
    expectType<string>(pcb.info().revision);
    expectType<string[]>(pcb.layers());
    const exports = [
      { format: "ipc2581", mode: "fabrication" },
      { format: "gerber", layoutTarget: "board-array", zip: true },
      { format: "svg", layer: "F.Cu" },
      { format: "png", layer: "F.Cu" },
      { format: "dxf" },
      { format: "bom" },
      { format: "cpl", side: "both", excludeDnp: true },
      { format: "ict", side: "bottom" },
      { format: "html" },
    ] satisfies ExportOptions[];
    for (const options of exports) {
      const files = pcb.export(options);
      expectType<ExportFile[]>(files);
      for (const file of files) {
        new TextDecoder().decode(file.data);
        new Blob([file.data], { type: file.mediaType });
      }
    }
    expectType<DfmReport>(pcb.checkDfm({ pdk: builtinPdks()[0].name }));
    const report = pcb.checkDfm({
      pdk: { name: "fab.toml", source: pdkToml },
      waivers: { name: "waivers.toml", source: waiverToml },
      layoutTarget: "board-array",
      generatedAt: "2026-08-30T12:00:00Z",
    });
    expectType<"pass" | "fail">(report.verdict);
    expectType<string>(report.input.sha256);
    expectType<string>(report.pdk.source);
    expectType<string>(report.pdk.profile);
    expectType<"executable" | "metadata_only">(report.pdk.profile_status);
    expectType<number | null>(report.pdk.support.copper_layers?.minimum ?? null);
    expectType<string | null>(report.pdk.defaults.outer_copper_weight);
    expectType<string>(report.layout.kind);
    expectType<string>(report.scene.passes[0].svg);
    expectType<number>(report.scene.bounds.min.x);
    for (const finding of report.findings) {
      expectType<number>(finding.sites[0].bounding_box.max.y);
    }
    if (report.waivers) expectType<string[]>(report.waivers.expired);
  } finally {
    decoded.free();
    pcb.free();
  }
}

export function rejectInvalidOptions(pcb: IpcDocument, bytes: Uint8Array): void {
  // @ts-expect-error Bytes use fromBytes.
  new IpcDocument(bytes);
  // @ts-expect-error Unknown import option.
  IpcDocument.fromBytes(bytes, { validateXml: true });
  // @ts-expect-error Export requires its format.
  pcb.export();
  // @ts-expect-error PNG requires a layer.
  pcb.export({ format: "png" });
  // @ts-expect-error zip applies only to Gerber.
  pcb.export({ format: "svg", layer: "F.Cu", zip: true });
  // @ts-expect-error Unsupported side.
  pcb.export({ format: "cpl", side: "left" });
  // @ts-expect-error Input layout names use kebab-case.
  pcb.checkDfm({ layoutTarget: "board_array" });
  // @ts-expect-error Waivers are text, not paths.
  pcb.checkDfm({ waivers: "waivers.toml" });
  // @ts-expect-error Custom PDKs require source text.
  pcb.checkDfm({ pdk: { name: "fab.toml" } });
  // @ts-expect-error Report verdict retains its type.
  expectType<number>(pcb.checkDfm().verdict);
  // @ts-expect-error Binary exports are typed, not any.
  expectType<string>(pcb.export({ format: "bom" })[0].data);
}
