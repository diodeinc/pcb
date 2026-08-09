macro_rules! insta_snapshot_name {
    () => {{
        fn marker() {}
        let mut function_path = std::any::type_name_of_val(&marker)
            .strip_suffix("::marker")
            .expect("snapshot marker must be a function");
        while let Some(path) = function_path.strip_suffix("::{{closure}}") {
            function_path = path;
        }
        let function = function_path
            .rsplit("::")
            .next()
            .expect("snapshot assertion must run in a function");
        let function = function.strip_prefix("test_").unwrap_or(function);
        let module = module_path!()
            .rsplit("::")
            .next()
            .expect("snapshot assertion must have a module");
        format!("{module}__{function}")
    }};
}
pub(crate) use insta_snapshot_name;

#[macro_use]
mod common;

mod assert;
mod canonical_snapshot;
mod component_properties;
mod enum_conversion;
mod input;
mod interface_templates;
mod interface_templates_snapshot;
mod load_diagnostics;
mod module_loader_attrs;
mod module_loading;
mod module_naming;
mod net;
mod part;
mod placeholder;
mod spice_model;
mod test;
mod test_physical;
mod test_physical_constructor_casting;
mod test_physical_negative_tolerance;
