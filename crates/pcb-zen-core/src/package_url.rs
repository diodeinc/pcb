use std::borrow::Cow;

/// Legacy package repository migrated to DiodeHub.
pub const LEGACY_REGISTRY_REPOSITORY: &str = "github.com/diodeinc/registry";
/// Canonical package repository on DiodeHub.
pub const CANONICAL_REGISTRY_REPOSITORY: &str = "code.diode.computer/diode/registry";

/// Canonicalize a package URL, dependency key, or package-prefix glob.
///
/// The rewrite is deliberately one-way and matches only the complete legacy
/// repository path, a slash-delimited child path, or a version selector on the
/// repository root.
pub fn canonicalize_package_reference(value: &str) -> Cow<'_, str> {
    if value == LEGACY_REGISTRY_REPOSITORY {
        return Cow::Borrowed(CANONICAL_REGISTRY_REPOSITORY);
    }

    let Some(suffix) = value.strip_prefix(LEGACY_REGISTRY_REPOSITORY) else {
        return Cow::Borrowed(value);
    };
    if !suffix.starts_with('/') && !suffix.starts_with('@') {
        return Cow::Borrowed(value);
    }

    Cow::Owned(format!("{CANONICAL_REGISTRY_REPOSITORY}{suffix}"))
}

/// Resolve a package reference literally, then retry its canonical identity.
pub fn resolve_package_reference<'a, T>(
    value: &'a str,
    mut resolve: impl FnMut(&str) -> Option<T>,
) -> Option<(Cow<'a, str>, T)> {
    if let Some(resolved) = resolve(value) {
        return Some((Cow::Borrowed(value), resolved));
    }

    let canonical = canonicalize_package_reference(value);
    if canonical.as_ref() == value {
        return None;
    }
    resolve(&canonical).map(|resolved| (canonical, resolved))
}

/// Whether a package reference belongs to the canonical registry repository.
pub fn is_canonical_registry_reference(value: &str) -> bool {
    value == CANONICAL_REGISTRY_REPOSITORY
        || value
            .strip_prefix(CANONICAL_REGISTRY_REPOSITORY)
            .is_some_and(|suffix| suffix.starts_with('/') || suffix.starts_with('@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_only_the_legacy_registry_boundary() {
        assert_eq!(
            canonicalize_package_reference(LEGACY_REGISTRY_REPOSITORY),
            CANONICAL_REGISTRY_REPOSITORY
        );
        assert_eq!(
            canonicalize_package_reference("github.com/diodeinc/registry/components/Foo/Foo.zen"),
            "code.diode.computer/diode/registry/components/Foo/Foo.zen"
        );
        assert_eq!(
            canonicalize_package_reference("github.com/diodeinc/registry/components/Foo@0.4"),
            "code.diode.computer/diode/registry/components/Foo@0.4"
        );
        assert_eq!(
            canonicalize_package_reference("github.com/diodeinc/registry@0.4"),
            "code.diode.computer/diode/registry@0.4"
        );
        assert_eq!(
            canonicalize_package_reference("github.com/diodeinc/registry-old/components/Foo"),
            "github.com/diodeinc/registry-old/components/Foo"
        );
        assert_eq!(
            canonicalize_package_reference("https://github.com/diodeinc/registry"),
            "https://github.com/diodeinc/registry"
        );
    }

    #[test]
    fn resolves_literal_before_registry_alias() {
        let legacy = "github.com/diodeinc/registry/components/Foo/Foo.zen";
        let canonical = "code.diode.computer/diode/registry/components/Foo/Foo.zen";

        assert_eq!(
            resolve_package_reference(legacy, |candidate| (candidate == legacy)
                .then_some("literal"))
            .map(|(matched, resolved)| (matched.into_owned(), resolved)),
            Some((legacy.to_string(), "literal"))
        );
        assert_eq!(
            resolve_package_reference(legacy, |candidate| {
                (candidate == canonical).then_some("canonical")
            })
            .map(|(matched, resolved)| (matched.into_owned(), resolved)),
            Some((canonical.to_string(), "canonical"))
        );
        assert_eq!(
            resolve_package_reference(canonical, |_| Some("literal")),
            Some((Cow::Borrowed(canonical), "literal"))
        );
        assert_eq!(resolve_package_reference(canonical, |_| None::<()>), None);
    }
}
