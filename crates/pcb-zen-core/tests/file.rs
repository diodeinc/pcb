#[macro_use]
mod common;

// Test basic file resolution
snapshot_eval!(file_resolves_relative_path, {
    "subdir/data.txt" => "test data",
    "test.zen" => r#"
        # Test resolving a relative path
        data_path = File("subdir/data.txt")
        print("Resolved path:", data_path)

        # Should return a package:// URI
        check(data_path == "package://test/subdir/data.txt", "File() should return package URI")
    "#
});

// Test that File() fails for non-existent files
snapshot_eval!(file_fails_for_nonexistent, {
    "test.zen" => r#"
        # This should fail because the file doesn't exist
        File("nonexistent.txt")
    "#
});

// Test that File() works for directories
snapshot_eval!(file_works_for_directory, {
    "subdir/file.txt" => "content",
    "test.zen" => r#"
        # File() should work with directories
        dir_path = File("subdir")
        check(dir_path == "package://test/subdir", "File() should return package URI for directory")
    "#
});

// Test file resolution from a subdirectory
snapshot_eval!(file_resolves_from_subdirectory, {
    "data.txt" => "root data",
    "subdir/data.txt" => "subdir data",
    "subdir/test.zen" => r#"
        # Should resolve relative to current file's directory
        local_data = File("data.txt")
        check(local_data == "package://test/subdir/data.txt", "Should resolve to local data.txt")

        # Can also use parent directory reference
        root_data = File("../data.txt")
        check(root_data == "package://test/data.txt", "Should resolve to root data.txt")
    "#
});

// Test File() with load() to ensure they use the same resolver
snapshot_eval!(file_consistent_with_load, {
    "lib/helper.zen" => r#"
        def get_path():
            return "lib/data.txt"
    "#,
    "lib/data.txt" => "library data",
    "test.zen" => r#"
        load("lib/helper.zen", "get_path")

        # File() should resolve paths the same way load() does
        lib_file = File("lib/data.txt")
        check(lib_file == "package://test/lib/data.txt", "Should resolve library file")

        # Should also work with the path from the loaded function
        path_from_lib = get_path()
        lib_file2 = File(path_from_lib)
        check(lib_file2 == "package://test/lib/data.txt", "Should resolve path from library")
    "#
});
