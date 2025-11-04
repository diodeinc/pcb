mod test_helpers;

#[test]
fn parse_kicad_generated_file() {
    let result = test_helpers::parse_compressed("tests/data/DM0002-IPC-2518.xml");

    match &result {
        Ok(doc) => {
            println!("Successfully parsed IPC-2581 file!");
            println!("Revision: {}", doc.revision());
            println!("Role: {}", doc.resolve(doc.content().role_ref));
            println!("Mode: {:?}", doc.content().function_mode.mode);
            println!("Number of layers: {}", doc.content().layer_refs.len());
            println!(
                "Number of colors: {}",
                doc.content().dictionary_color.entries.len()
            );
            println!(
                "Number of line descs: {}",
                doc.content().dictionary_line_desc.entries.len()
            );
            println!(
                "Number of standard primitives: {}",
                doc.content().dictionary_standard.entries.len()
            );

            // Check a few entries
            if let Some(first_color) = doc.content().dictionary_color.entries.first() {
                println!("First color ID: {}", doc.resolve(first_color.id));
                println!(
                    "  RGB: ({}, {}, {})",
                    first_color.color.r, first_color.color.g, first_color.color.b
                );
            }

            if let Some(first_std) = doc.content().dictionary_standard.entries.first() {
                println!("First standard primitive ID: {}", doc.resolve(first_std.id));
                println!("  Type: {:?}", first_std.primitive);
            }
        }
        Err(e) => {
            eprintln!("Error: {:?}", e);
            panic!("Failed to parse file");
        }
    }

    assert!(result.is_ok());
}
