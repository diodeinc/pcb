#!/usr/bin/env bash
# Software smoke of checked-in main layouts, not Zen/layout sync or qualification.
set -eu
root=${1:?usage: smoke-demo.sh CLONED_REPOSITORY_DIRECTORY OUTPUT_DIRECTORY}
out=${2:?usage: smoke-demo.sh CLONED_REPOSITORY_DIRECTORY OUTPUT_DIRECTORY}
mkdir -p "$out"/{exports,provenance,fixtures,logs,replay}
version=$(kicad-cli version)
printf 'board\texport\textraction\treplay\n' > "$out/outcomes.tsv"
for name in Bramble Demeter Feign Governor Marlow Renfield Seward; do
    repo="$root/$name"
    layout=layout/layout.kicad_pcb
    if [ "$name" = Bramble ]; then layout=layout/DM0003/layout.kicad_pcb; fi
    revision=$(git -C "$repo" rev-parse HEAD)
    url=$(git -C "$repo" remote get-url origin)
    layout_hash=$(sha256sum "$repo/$layout" | cut -d ' ' -f 1)
    id="demo-${name,,}"
    jq -n --arg repository "$url" --arg revision "$revision" --arg path "$layout" \
        --arg version "$version" --arg hash "$layout_hash" --arg name "$name" '{
        repository:$repository, revision:$revision, path:$path, sha256:null,
        extraction:"filled by canonical extractor",
        limitations:[
            ("Checked-in layout SHA-256: " + $hash),
            ("Exported with kicad-cli " + $version + " pcb export ipc2581 --version C --units mm --precision 6"),
            "Source SHA-256 identifies generated XML; KiCad export timestamps may change across runs",
            "Main checked-in layout only; no Zen rebuild, layout sync, DRC, physical validation or manufacturing qualification",
            (if $name == "Seward" then "Source README explicitly says latest hardware changes still need layout synchronization and bench validation" else "Layout/source synchronization is not established by this smoke" end)
        ]}' > "$out/provenance/$id.json"
    if ! kicad-cli pcb export ipc2581 --version C --units mm --precision 6 \
        -o "$out/exports/$name.xml" "$repo/$layout" > "$out/logs/$name-export.log" 2>&1; then
        printf '%s\tfailed\tnot_run\tnot_run\n' "$name" >> "$out/outcomes.tsv"
        continue
    fi
    if ! cargo run --quiet -p pcb-corpus --features extract --example extract -- \
        "$out/exports/$name.xml" "$out/provenance/$id.json" "$id" "$out/fixtures/$id.json.zst" \
        > "$out/logs/$name-extraction.log" 2>&1; then
        printf '%s\tcompleted\tfailed\tnot_run\n' "$name" >> "$out/outcomes.tsv"
        continue
    fi
    if cargo run --quiet -p pcb-corpus -- "$out/replay/$name" "$out/fixtures/$id.json.zst" \
        > "$out/logs/$name-replay.log" 2>&1; then
        printf '%s\tcompleted\tcompleted\tcompleted\n' "$name" >> "$out/outcomes.tsv"
    else
        printf '%s\tcompleted\tcompleted\tfailed\n' "$name" >> "$out/outcomes.tsv"
    fi
done
cat "$out/outcomes.tsv"
! grep -q 'failed' "$out/outcomes.tsv"
