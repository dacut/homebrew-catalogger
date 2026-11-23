use {
    homebrew_catalog_classifier::{Error, UsesFromMacos, find_formula, load_formulae, read_formula_file},
    serde_json::json,
    std::{fs, path::Path},
};

fn formula(name: &str, desc: &str) -> serde_json::Value {
    json!({
        "name": name,
        "full_name": name,
        "tap": "homebrew/core",
        "oldnames": [format!("{name}-old")],
        "aliases": [],
        "desc": desc,
        "license": null,
        "homepage": format!("https://example.com/{name}"),
        "versions": {"stable": "1.0", "head": null, "bottle": true},
        "build_dependencies": ["rust"],
        "dependencies": [],
        "uses_from_macos": ["zlib", {"perl": "build"}],
        "keg_only_reason": {"reason": ":provided_by_macos", "explanation": ""},
        "executables": [name],
        "analytics": {"install": {"30d": {name: 1}}},
    })
}

fn write(path: &Path, value: &serde_json::Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn parses_real_shaped_formula() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("aichat.json");
    write(&file, &formula("aichat", "All-in-one AI-Powered CLI Chat & Copilot"));

    let formulae = read_formula_file(&file).unwrap();
    assert_eq!(formulae.len(), 1);
    let f = &formulae[0];
    assert_eq!(f.name, "aichat");
    assert_eq!(f.versions.stable.as_deref(), Some("1.0"));
    assert_eq!(f.license, None);
    assert_eq!(f.uses_from_macos[0], UsesFromMacos::Name("zlib".into()));
    assert_eq!(f.uses_from_macos[1].names(), vec!["perl"]);
    assert!(f.is_named("aichat-old"));
}

#[test]
fn unmodeled_fields_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("aichat.json");
    let original = formula("aichat", "AI chat");
    write(&file, &original);

    let parsed = read_formula_file(&file).unwrap().remove(0);
    assert!(parsed.extra.contains_key("analytics"));
    assert!(!parsed.extra.contains_key("name"), "modeled fields are not duplicated in `extra`");

    let serialized = serde_json::to_value(&parsed).unwrap();
    for (key, value) in original.as_object().unwrap() {
        assert_eq!(&serialized[key], value, "field {key:?} did not round-trip");
    }
}

#[test]
fn loads_cache_root_with_bulk_and_per_formula_files() {
    let cache = tempfile::tempdir().unwrap();
    let api = cache.path().join("api");

    // Bulk JWS file containing two formulae; the per-formula file overrides one of them.
    let payload =
        serde_json::to_string(&json!([formula("jq", "old desc"), formula("wget", "Internet file retriever")])).unwrap();
    write(&api.join("formula.jws.json"), &json!({"payload": payload, "signatures": []}));
    write(&api.join("formula").join("jq.json"), &formula("jq", "Lightweight JSON processor"));
    write(&api.join("formula").join("aichat.json"), &formula("aichat", "AI chat"));
    fs::write(api.join("formula").join("jq.json.etag"), "\"abc\"").unwrap();

    for path in [cache.path(), api.as_path()] {
        let formulae = load_formulae(path).unwrap();
        let names: Vec<_> = formulae.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["aichat", "jq", "wget"]);
        assert_eq!(formulae[1].desc.as_deref(), Some("Lightweight JSON processor"));
    }
}

#[test]
fn loads_plain_directory_and_array_file() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a.json"), &formula("a", "A"));
    write(&dir.path().join("b.json"), &json!([formula("b", "B"), formula("c", "C")]));
    fs::write(dir.path().join("notes.txt"), "ignored").unwrap();

    let names: Vec<_> = load_formulae(dir.path()).unwrap().into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["a", "b", "c"]);
}

#[test]
fn finds_formula_by_name_alias_and_oldname() {
    let cache = tempfile::tempdir().unwrap();
    let api = cache.path().join("api");
    write(&api.join("formula").join("zeromq.json"), &formula("zeromq", "High-performance messaging"));
    write(&api.join("formula").join("jq.json"), &formula("jq", "JSON processor"));
    fs::write(api.join("formula_aliases.txt"), "0mq|zeromq\n").unwrap();

    assert_eq!(find_formula("zeromq", cache.path()).unwrap().name, "zeromq");
    assert_eq!(find_formula("0mq", cache.path()).unwrap().name, "zeromq");
    assert_eq!(find_formula("jq-old", &api).unwrap().name, "jq");
    assert!(matches!(find_formula("nope", cache.path()), Err(Error::FormulaNotFound(_))));
    assert!(matches!(find_formula("../jq", cache.path()), Err(Error::FormulaNotFound(_))));
}

#[test]
fn rejects_unrecognized_and_invalid_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.json");
    write(&file, &json!({"not": "a formula"}));
    assert!(matches!(read_formula_file(&file), Err(Error::UnrecognizedMetadata { .. })));

    // Array entries get the same check as a standalone object.
    for bad in [json!([{}]), json!([formula("ok", "fine"), {"not": "a formula"}]), json!([{"name": ""}]), json!([1])] {
        write(&file, &bad);
        match read_formula_file(&file) {
            Err(Error::UnrecognizedMetadata {
                reason,
                ..
            }) => assert!(reason.contains("entry"), "{reason}"),
            other => panic!("{bad} should be rejected, got {other:?}"),
        }
    }

    fs::write(&file, "{").unwrap();
    assert!(matches!(read_formula_file(&file), Err(Error::Json { .. })));

    assert!(matches!(load_formulae(dir.path().join("missing")), Err(Error::Io { .. })));
}
