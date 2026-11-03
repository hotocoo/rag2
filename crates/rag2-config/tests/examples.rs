//! Every shipped example profile parses, applies each of its environment overlays, and validates
//! without errors. Keeps the documentation honest.

use rag2_config::{Severity, parse_profile_value, resolve, validate};

#[test]
fn example_profiles_are_valid() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/profiles");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("examples dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let value =
            parse_profile_value(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let base = resolve(&value, None).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let envs: Vec<Option<String>> = std::iter::once(None)
            .chain(base.environments.keys().cloned().map(Some))
            .collect();
        for env in envs {
            let p = resolve(&value, env.as_deref())
                .unwrap_or_else(|e| panic!("{} [{env:?}]: {e}", path.display()));
            let errors: Vec<_> = validate(&p)
                .into_iter()
                .filter(|i| i.severity == Severity::Error)
                .collect();
            assert!(
                errors.is_empty(),
                "{} [{env:?}]: {errors:?}",
                path.display()
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 5,
        "expected the shipped examples, found {checked}"
    );
}

#[test]
fn the_container_profiles_are_valid() {
    for name in ["profile.container.yaml", "profile.container.d1.yaml"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy")
            .join(name);
        let p = resolve(
            &parse_profile_value(&std::fs::read_to_string(&path).expect("read")).expect("parse"),
            None,
        )
        .expect("resolve");
        let errors: Vec<_> = validate(&p)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
}
