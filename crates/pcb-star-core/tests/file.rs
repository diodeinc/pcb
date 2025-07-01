#[macro_use]
mod common;

// Test basic file resolution
snapshot_eval!(file_resolves_relative_path, {
    "subdir/data.txt" => "test data",
    "test.star" => r#"
        # Test resolving a relative path
        data_path = File("subdir/data.txt")
        print("Resolved path:", data_path)

        # Should return an absolute path
        check(data_path == "/subdir/data.txt", "File() should return absolute path")
    "#
});

// Test that File() fails for non-existent files
snapshot_eval!(file_fails_for_nonexistent, {
    "test.star" => r#"
        # This should fail because the file doesn't exist
        File("nonexistent.txt")
    "#
});

// Test that File() works for directories
snapshot_eval!(file_works_for_directory, {
    "subdir/file.txt" => "content",
    "test.star" => r#"
        # File() should work with directories
        dir_path = File("subdir")
        check(dir_path == "/subdir", "File() should return absolute path for directory")
    "#
});

// Test file resolution from a subdirectory
snapshot_eval!(file_resolves_from_subdirectory, {
    "data.txt" => "root data",
    "subdir/data.txt" => "subdir data",
    "subdir/test.star" => r#"
        # Should resolve relative to current file's directory
        local_data = File("data.txt")
        check(local_data == "/subdir/data.txt", "Should resolve to local data.txt")

        # Can also use parent directory reference
        root_data = File("../data.txt")
        check(root_data == "/data.txt", "Should resolve to root data.txt")
    "#
});

// Test that File() works with absolute paths
snapshot_eval!(file_handles_absolute_paths, {
    "data.txt" => "test data",
    "test.star" => r#"
        # Should work with absolute paths too
        data_path = File("/data.txt")
        check(data_path == "/data.txt", "Should handle absolute paths")
    "#
});

// Test File() with load() to ensure they use the same resolver
snapshot_eval!(file_consistent_with_load, {
    "lib/helper.star" => r#"
        def get_path():
            return "lib/data.txt"
    "#,
    "lib/data.txt" => "library data",
    "test.star" => r#"
        load("lib/helper.star", "get_path")

        # File() should resolve paths the same way load() does
        lib_file = File("lib/data.txt")
        check(lib_file == "/lib/data.txt", "Should resolve library file")

        # Should also work with the path from the loaded function
        path_from_lib = get_path()
        lib_file2 = File(path_from_lib)
        check(lib_file2 == "/lib/data.txt", "Should resolve path from library")
    "#
});
