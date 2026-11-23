//! High-voltage clearance in the Specctra DSN handed to FreeRouting.
//!
//! KiCad exports one clearance per net class, so a high-voltage class
//! carries the coated clearance its tracks and tented vias need, and the
//! exposed one its pads need — held by the board's custom rules — is lost.
//! FreeRouting resolves clearance by class pair, from a matrix the DSN can
//! spell out, and lets a placed pin name its own class. [`expose`] gives
//! every net class a clearance class for its tracks and vias and one for
//! its pads, and writes the matrix: the exposed clearance wherever a pad
//! meets high-voltage copper, or a high-voltage pad meets anything; the
//! larger class clearance elsewhere.

use std::path::Path;

use anyhow::{Context, Result};

use crate::bed::HvClearance;
use crate::emit::HV_CLASSES;

const SCRIPT: &str = r#"
import re
import sys

dsn_path, exposed, hot_names = sys.argv[1], float(sys.argv[2]), sys.argv[3].split(",")
src = open(dsn_path).read()
if not re.search(r"\(unit um\)", src):
    sys.exit("DSN unit is not um")
# FreeRouting approximates round pads by polygons, cutting up to
# r(1 - cos 22.5°) off a radius: 0.08 mm on a 2.08 mm pogo pad.
EXPOSED = round((exposed + 0.1) * 1000)
TOKEN = r'"[^"]*"|[^\s()]+'


def block_end(start):
    """End of the parenthesized block opening at `start`."""
    depth, quoted = 0, False
    for i in range(start, len(src)):
        ch = src[i]
        if ch == '"':
            quoted = not quoted
        elif not quoted and ch == "(":
            depth += 1
        elif not quoted and ch == ")":
            depth -= 1
            if depth == 0:
                return i + 1
    sys.exit("unbalanced DSN block")


# Net classes: a clearance class for tracks (`nK`) and one for pads
# (`nKpad`), each with its class's clearance. A `wire_wire` pair in the
# class rule has FreeRouting derive the class's via class (`nK-via`)
# from the track class's row, matrix included, and build the class's
# vias on it; a class without one routes its vias in the default class.
base, hot, net_class = {}, set(), {}
default = None
blocks = [(m.start(), block_end(m.start())) for m in re.finditer(r"\(class\s", src)]
for k, (start, end) in reversed(list(enumerate(blocks))):
    block = src[start:end]
    head = block[len("(class") : block.index("(", 1)]
    first = re.search(TOKEN, head)
    name, *nets = [t.strip('"') for t in re.findall(TOKEN, head)]
    head = head[first.end() :]
    clearance = re.search(r"\(rule.*?\(clearance ([\d.]+)\)", block, re.S)
    width = re.search(r"\(width ([\d.]+)\)", block)
    via = re.search(r"\(use_via (" + TOKEN + r")\)", block)
    if not (clearance and width and via):
        sys.exit(f"net class {name} lacks a clearance, width or via")
    trace, pad = f"n{k}", f"n{k}pad"
    base[trace] = base[pad] = float(clearance.group(1))
    if name.split(",")[0] in hot_names:
        hot |= {trace, pad}
    if name == "kicad_default":
        default = pad
    for net in nets:
        net_class[net] = pad
    rule = f"(rule (width {width.group(1)}) (clearance {clearance.group(1)}) (clearance {clearance.group(1)} (type wire_wire)))"
    src = src[:start] + f"(class {trace}{head}(circuit (use_via {via.group(1)})) {rule})" + src[end:]

# Pins: each takes its net's pad class.
pin_class = {}
for net, pins in re.findall(r"\(net (" + TOKEN + r")\s+\(pins ([^)]*)\)", src):
    cls = net_class.get(net.strip('"'), default)
    if cls is None:
        continue
    for pin in pins.split():
        ref, _, num = pin.strip('"').rpartition("-")
        pin_class.setdefault(ref, []).append((num, cls))


# Pins on no net (an unused bed land, a fiducial) are still bare pads:
# they take the default class's pad class, so a high-voltage track keeps
# the exposed clearance from them too, as the board's rules require.
image_pins = {}
for m in re.finditer(r"\(image (" + TOKEN + r")", src):
    body = src[m.start() : block_end(m.start())]
    nums = re.findall(r"\(pin (?:" + TOKEN + r")\s+(?:\(rotate [^)]*\)\s+)?(" + TOKEN + r")\s", body)
    image_pins[m.group(1).strip('"')] = [n.strip('"') for n in nums]
ref_image = {}
for m in re.finditer(r"\(component (" + TOKEN + r")", src):
    body = src[m.start() : block_end(m.start())]
    for ref in re.findall(r"\(place (" + TOKEN + r")", body):
        ref_image[ref.strip('"')] = m.group(1).strip('"')
if default is not None:
    for ref, image in ref_image.items():
        have = {num for num, _ in pin_class.get(ref, [])}
        for num in image_pins.get(image, []):
            if num not in have:
                pin_class.setdefault(ref, []).append((num, default))


def place(m):
    extra = "".join(f" (pin {num} (clearance_class {cls}))" for num, cls in pin_class.get(m.group(1).strip('"'), []))
    return m.group(0)[:-1] + extra + ")"


src = re.sub(r"\(place (" + TOKEN + r") [^()\n]*(?:\([^()]*\))*\)", place, src)


def value(a, b):
    if (a.endswith("pad") or b.endswith("pad")) and (a in hot or b in hot):
        return max(EXPOSED, base[a], base[b])
    return max(base[a], base[b])


# After the structure's own rules: its default clearance resets the matrix.
rules = "".join(f"  (clearance {value(a, b):g} (type {a}_{b}))\n    " for a in base for b in base)
rule = src.index("(rule", src.index("(structure"))
end = block_end(rule) - 1
src = src[:end] + rules + src[end:]

depth = 0
for ch in re.sub(r'"[^"]*"', "", src.replace('(string_quote ")', "")):
    depth += {"(": 1, ")": -1}.get(ch, 0)
    if depth < 0:
        break
if depth != 0:
    sys.exit("rewritten DSN is unbalanced")
open(dsn_path, "w").write(src)
"#;

/// Rewrite the DSN at `dsn_path` in place so FreeRouting keeps `hv`'s
/// exposed clearance at bare copper and its coated one elsewhere.
pub fn expose(dsn_path: &Path, hv: HvClearance) -> Result<()> {
    pcb_kicad::PythonScriptBuilder::new(SCRIPT)
        .arg(dsn_path.to_string_lossy())
        .arg(hv.exposed.to_string())
        .arg(HV_CLASSES.join(","))
        .run()
        .context("hold high-voltage clearance at exposed copper in the DSN")
}
