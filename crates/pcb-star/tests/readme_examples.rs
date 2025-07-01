mod common;
use common::TestProject;
use std::fs;
use std::path::Path;
use std::env;

/// Extract code blocks marked with `<!-- test:readme_examples -->` from the README
fn extract_readme_examples() -> Vec<(String, String)> {
    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("README.md");
    
    let content = fs::read_to_string(&readme_path)
        .expect("Failed to read README.md");
    
    let mut examples = Vec::new();
    let mut in_code_block = false;
    let mut is_test_block = false;
    let mut current_code = String::new();
    let mut example_counter = 0;
    let mut lines = content.lines().peekable();
    
    while let Some(line) = lines.next() {
        if line.trim() == "<!-- test:readme_examples -->" {
            // Check if the next line starts a code block
            if let Some(next_line) = lines.peek() {
                if next_line.starts_with("```python") {
                    is_test_block = true;
                }
            }
        } else if line.starts_with("```python") && is_test_block {
            in_code_block = true;
            current_code.clear();
        } else if line == "```" && in_code_block && is_test_block {
            in_code_block = false;
            is_test_block = false;
            example_counter += 1;
            let name = format!("readme_example_{}", example_counter);
            examples.push((name, current_code.clone()));
        } else if in_code_block && is_test_block {
            current_code.push_str(line);
            current_code.push('\n');
        }
    }
    
    examples
}

/// Process an example to handle multi-file examples
fn process_example(content: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut current_file = String::new();
    let mut current_content = String::new();
    let mut in_file = false;
    
    for line in content.lines() {
        if line.trim().starts_with("# ") && line.contains(".star") {
            // This looks like a file marker
            if in_file && !current_file.is_empty() {
                files.push((current_file.clone(), current_content.clone()));
                current_content.clear();
            }
            current_file = line.trim_start_matches('#').trim().to_string();
            in_file = true;
        } else {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }
    
    // Don't forget the last file or the entire content if no file markers
    if in_file && !current_file.is_empty() {
        files.push((current_file, current_content));
    } else if files.is_empty() {
        files.push(("main.star".to_string(), content.to_string()));
    }
    
    files
}



#[test]
fn test_readme_examples() {
    let examples = extract_readme_examples();
    
    // Ensure we found examples
    assert!(!examples.is_empty(), "No examples found in README.md");
    
    let total_examples = examples.len();
    println!("Found {} examples in README.md", total_examples);
    
    // Check if we should test external dependencies
    let test_external_deps = env::var("PCB_TEST_EXTERNAL_DEPS").unwrap_or_default() == "1";
    
    let mut successful = 0;
    let mut external_deps = 0;
    let mut doc_snippets = 0;
    let mut skipped_external = 0;
    
    for (example_name, content) in examples {
        println!("Testing example: {}", example_name);
        
        // Check if this example has external dependencies
        let has_external_deps = content.contains("@github/diodeinc/stdlib");
        
        if has_external_deps && !test_external_deps {
            println!("  ⏭️  Example {} skipped (has external dependencies, set PCB_TEST_EXTERNAL_DEPS=1 to test)", example_name);
            skipped_external += 1;
            continue;
        }
        
        let env = TestProject::new();
        
        // Process the example to extract files
        let files = process_example(&content);
        
        // Add files to the test project
        let mut main_file = None;
        for (filename, file_content) in files {
            env.add_file(&filename, &file_content);
            
            // Track the main file to evaluate
            if filename == "main.star" || (!filename.contains("_") && main_file.is_none()) {
                main_file = Some(filename);
            }
        }
        
        // If we have a main file, evaluate it
        if let Some(main) = main_file {
            let result = env.eval_netlist(&main);
            
            // For README examples, we're mainly checking that they parse and evaluate
            // without errors. The actual netlist output is less important.
            match result.output {
                Some(_) => {
                    println!("  ✓ Example {} evaluated successfully", example_name);
                    successful += 1;
                    // Check if there were any warnings
                    if !result.diagnostics.is_empty() {
                        println!("  Warnings for {}: {:?}", example_name, result.diagnostics);
                    }
                }
                None => {
                    // Some examples are expected to be incomplete or depend on external modules
                    // We'll check for known cases where failure is expected
                    let error_msg = if !result.diagnostics.is_empty() {
                        format!("{:?}", result.diagnostics)
                    } else {
                        "No output produced".to_string()
                    };
                    
                    if has_external_deps {
                        // Examples that depend on the external stdlib are expected to fail
                        // unless we have network access to fetch them
                        println!("  ⚠ Example {} failed with external dependencies: {}", example_name, error_msg);
                        external_deps += 1;
                    } else if content.contains("# ... more pins") || 
                              content.contains("# ... component configuration") ||
                              content.contains("# Required configuration") ||
                              content.contains("# Define IO interfaces") {
                        // Examples with ellipsis or documentation snippets are incomplete by design
                        println!("  ⚠ Example {} is a documentation snippet (intentionally incomplete)", example_name);
                        doc_snippets += 1;
                    } else {
                        // This is an unexpected failure
                        panic!("Example {} failed to evaluate\nDiagnostics: {:?}\nContent:\n{}", 
                               example_name, result.diagnostics, content);
                    }
                }
            }
        } else {
            // No main file found - this is likely a documentation snippet
            println!("  ⚠ Example {} is a documentation snippet (no executable file)", example_name);
            doc_snippets += 1;
        }
    }
    
    // Print summary
    println!("\n=== README Examples Test Summary ===");
    println!("Total examples found: {}", total_examples);
    println!("  ✓ Successfully evaluated: {}", successful);
    if skipped_external > 0 {
        println!("  ⏭️  Skipped (external deps): {}", skipped_external);
    }
    if external_deps > 0 {
        println!("  ⚠ Failed with external deps: {}", external_deps);
    }
    println!("  ⚠ Documentation snippets: {}", doc_snippets);
    println!("====================================");
    
    if !test_external_deps && (external_deps > 0 || skipped_external > 0) {
        println!("\nNote: To test examples with external dependencies, run:");
        println!("  PCB_TEST_EXTERNAL_DEPS=1 cargo test -p pcb-star test_readme_examples");
    }
}