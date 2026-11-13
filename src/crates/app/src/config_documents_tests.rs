use std::path::Path;
use std::time::SystemTime;

use serde_json::json;
use stache_sdk::config::PathSegment;
use stache_sdk::{DiscardLogger, EventBus, LogFilter, Registry};

use super::*;
use crate::catalog::catalog;

const TILING: &str = r#"#:schema https://example.invalid/tiling.schema.json

# Tiling is opt-in.
enabled = false

[gaps]
inner = 10 # px
outer = 14

[[workspaces]]
name = "terminal"
layout = "monocle"

[[workspaces]]
name = "coding"
layout = "dwindle"
"#;

struct NoMainThread;

impl stache_sdk::MainThreadExecutor for NoMainThread {
    fn is_main_thread(&self) -> bool { false }

    fn execute(
        &self,
        _: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), stache_sdk::ThreadError> {
        Ok(())
    }
}

struct AllowAll;

impl stache_sdk::PermissionProvider for AllowAll {
    fn status(&self, _: stache_sdk::Permission) -> stache_sdk::PermissionStatus {
        stache_sdk::PermissionStatus::Granted
    }

    fn request(
        &self,
        _: stache_sdk::Permission,
    ) -> Result<stache_sdk::PermissionStatus, stache_sdk::PermissionError> {
        Ok(stache_sdk::PermissionStatus::Granted)
    }
}

struct NoWindows;

impl stache_sdk::WindowBackend for NoWindows {
    fn open(
        &self,
        _: ModuleId,
        _: &stache_sdk::WindowRequest,
    ) -> Result<stache_sdk::WindowId, stache_sdk::WindowError> {
        Ok(stache_sdk::WindowId::new(1))
    }

    fn focus(&self, _: stache_sdk::WindowId) -> Result<(), stache_sdk::WindowError> { Ok(()) }

    fn close(&self, _: stache_sdk::WindowId) {}

    fn release_owner(&self, _: ModuleId) {}
}

struct Rig {
    _root: tempfile::TempDir,
    store: ConfigStore,
}

impl Rig {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("root");
        let paths = ConfigPaths::new(root.path().join("config"));
        fs::create_dir_all(paths.directory()).expect("config dir");
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            Arc::new(AllowAll),
            Arc::new(NoWindows),
        ));
        let store = ConfigStore::new(
            registry,
            catalog().expect("catalog"),
            paths,
            Arc::new(LogFilter::default()),
        );
        Self { _root: root, store }
    }

    fn path(&self, file: &str) -> std::path::PathBuf { self.store.paths().directory().join(file) }

    fn write(&self, file: &str, text: &str) { fs::write(self.path(file), text).expect("write"); }

    fn read(&self, file: &str) -> String { fs::read_to_string(self.path(file)).expect("read") }

    fn mtime(&self, file: &str) -> SystemTime {
        fs::metadata(self.path(file)).expect("metadata").modified().expect("mtime")
    }

    fn document(&self, file: &str) -> ConfigDocument {
        match read_document(self.store.catalog(), self.store.paths(), file) {
            ConfigDocumentOutcome::Document { document } => *document,
            ConfigDocumentOutcome::UnknownFile { .. } => panic!("unknown file"),
        }
    }

    fn edit(&self, file: &str, base_hash: Option<String>, op: ConfigEditOp) -> ConfigEditOutcome {
        run_edit(&self.store, &ConfigEditRequest {
            file: file.to_owned(),
            base_hash,
            op,
        })
        .outcome
    }
}

fn key(name: &str) -> PathSegment { PathSegment::Key(name.to_owned()) }

fn hash_of(path: &Path) -> Option<String> { fs::read(path).ok().map(|bytes| content_hash(&bytes)) }

#[test]
fn an_edit_commits_surgically_and_returns_the_new_hash() {
    let rig = Rig::new();
    rig.write("tiling.toml", TILING);
    let base = hash_of(&rig.path("tiling.toml"));
    let outcome = rig.edit("tiling.toml", base, ConfigEditOp::Set {
        path: vec![key("gaps"), key("inner")],
        value: json!(12),
    });
    let ConfigEditOutcome::Committed { hash, reload } = outcome else {
        panic!("expected a commit: {outcome:?}");
    };
    assert_eq!(
        rig.read("tiling.toml"),
        TILING.replacen("inner = 10", "inner = 12", 1)
    );
    assert_eq!(Some(hash), hash_of(&rig.path("tiling.toml")));
    // No module is bound in this rig; the reload reports why.
    assert!(matches!(reload, ConfigReloadView::Rejected { .. }));
}

#[test]
fn invalid_values_are_refused_with_the_catalog_message_and_field() {
    let rig = Rig::new();
    rig.write("tiling.toml", TILING);
    let before = rig.mtime("tiling.toml");
    let base = hash_of(&rig.path("tiling.toml"));
    let cases = [
        (vec![key("enabled")], json!("wide"), "enabled"),
        // `gaps` is an untagged union, so the catalog names the union.
        (vec![key("gaps"), key("inner")], json!(-1), "gaps"),
        (
            vec![key("workspaces"), PathSegment::Index(0), key("layout")],
            json!("spiral"),
            "workspaces[0].layout",
        ),
        // A validation error without a field falls back to the edited path.
        (
            vec![key("workspaces"), PathSegment::Index(1), key("name")],
            json!("terminal"),
            "workspaces[1].name",
        ),
    ];
    for (path, value, field) in cases {
        let outcome = rig.edit("tiling.toml", base.clone(), ConfigEditOp::Set { path, value });
        let ConfigEditOutcome::Invalid { diagnostic } = outcome else {
            panic!("expected invalid: {outcome:?}");
        };
        assert!(!diagnostic.message.is_empty());
        assert_eq!(
            diagnostic.field.as_deref(),
            Some(field),
            "{}",
            diagnostic.message
        );
        assert_eq!(rig.read("tiling.toml"), TILING);
        assert_eq!(rig.mtime("tiling.toml"), before);
    }
}

#[test]
fn stale_symlinked_invalid_and_unknown_files_are_refused() {
    let rig = Rig::new();
    rig.write("tiling.toml", TILING);
    let op = ConfigEditOp::Set {
        path: vec![key("gaps"), key("inner")],
        value: json!(1),
    };
    let outcome = rig.edit("tiling.toml", Some("0000000000000000".to_owned()), op.clone());
    assert_eq!(outcome, ConfigEditOutcome::Stale {
        hash: hash_of(&rig.path("tiling.toml"))
    });
    assert_eq!(rig.read("tiling.toml"), TILING);
    // A missing file is stale against any hash, and an existing one against none.
    assert!(matches!(
        rig.edit("tiling.toml", None, op.clone()),
        ConfigEditOutcome::Stale { .. }
    ));
    assert!(matches!(
        rig.edit("bar.toml", Some("1".to_owned()), op.clone()),
        ConfigEditOutcome::Stale { hash: None }
    ));

    rig.write("keepAwake.toml", "enabled = \n");
    let outcome = rig.edit(
        "keepAwake.toml",
        hash_of(&rig.path("keepAwake.toml")),
        ConfigEditOp::Set {
            path: vec![key("enabled")],
            value: json!(true),
        },
    );
    assert!(matches!(outcome, ConfigEditOutcome::Refused {
        reason: ConfigEditRefusal::CurrentInvalid,
        ..
    }));
    assert_eq!(rig.read("keepAwake.toml"), "enabled = \n");

    let target = rig.path("real.toml");
    fs::write(&target, "enabled = false\n").expect("target");
    std::os::unix::fs::symlink(&target, rig.path("notunes.toml")).expect("symlink");
    let outcome = rig.edit("notunes.toml", hash_of(&target), ConfigEditOp::Set {
        path: vec![key("enabled")],
        value: json!(true),
    });
    assert!(matches!(outcome, ConfigEditOutcome::Refused {
        reason: ConfigEditRefusal::Symlink,
        ..
    }));
    assert_eq!(fs::read_to_string(&target).expect("target"), "enabled = false\n");

    let outcome = rig.edit("../tiling.toml", None, op);
    assert!(matches!(outcome, ConfigEditOutcome::Refused {
        reason: ConfigEditRefusal::UnknownFile,
        ..
    }));
    let outcome = rig.edit(
        "tiling.toml",
        hash_of(&rig.path("tiling.toml")),
        ConfigEditOp::Move {
            path: vec![key("workspaces")],
            from: 0,
            to: 5,
        },
    );
    assert!(matches!(outcome, ConfigEditOutcome::Refused {
        reason: ConfigEditRefusal::Path,
        ..
    }));
}

#[test]
fn an_edit_to_a_missing_file_creates_it_from_the_template() {
    let rig = Rig::new();
    let outcome = rig.edit("tiling.toml", None, ConfigEditOp::Set {
        path: vec![key("gaps"), key("inner")],
        value: json!(7),
    });
    assert!(
        matches!(outcome, ConfigEditOutcome::Committed { .. }),
        "{outcome:?}"
    );
    let text = rig.read("tiling.toml");
    assert!(
        text.lines().next().is_some_and(|line| line.starts_with("#:schema ")),
        "{text}"
    );
    let template = rig.store.catalog().template(stache_tiling::TilingConfig::ID).expect("template");
    assert!(text.len() > template.len() / 2, "{text}");
    let document = rig.document("tiling.toml");
    assert_eq!(document.state, ConfigDocumentState::Valid);
    assert_eq!(document.value.expect("value")["gaps"]["inner"], json!(7));
}

#[test]
fn documents_report_missing_valid_invalid_and_symlinked_files() {
    let rig = Rig::new();
    let missing = rig.document("tiling.toml");
    assert_eq!(missing.state, ConfigDocumentState::Missing);
    assert_eq!(missing.hash, None);
    assert_eq!(missing.value, None);
    assert_eq!(missing.default["gaps"]["inner"], json!(0));
    assert!(missing.schema.get("properties").is_some(), "{}", missing.schema);
    assert_eq!(missing.diagnostic, None);

    rig.write("tiling.toml", "enabled = true\nmystery = 1\n");
    let valid = rig.document("tiling.toml");
    assert_eq!(valid.state, ConfigDocumentState::Valid);
    assert_eq!(
        valid.value,
        Some(json!({ "enabled": true, "mystery": 1 })),
        "only keys in the file"
    );
    assert_eq!(valid.hash, hash_of(&rig.path("tiling.toml")));
    assert_eq!(valid.warnings, vec!["mystery".to_owned()]);

    rig.write("tiling.toml", "enabled = true\n[gaps\n");
    let invalid = rig.document("tiling.toml");
    assert_eq!(invalid.state, ConfigDocumentState::Invalid);
    assert_eq!(invalid.value, None);
    assert!(invalid.hash.is_some());
    let diagnostic = invalid.diagnostic.expect("diagnostic");
    assert_eq!(diagnostic.line, Some(2));
    assert!(diagnostic.column.is_some());

    rig.write("tiling.toml", "enabled = \"x\"\n");
    let typed = rig.document("tiling.toml").diagnostic.expect("diagnostic");
    assert_eq!(typed.field.as_deref(), Some("enabled"));

    let target = rig.path("elsewhere.toml");
    fs::write(&target, "enabled = true\n").expect("target");
    std::os::unix::fs::symlink(&target, rig.path("bar.toml")).expect("symlink");
    let linked = rig.document("bar.toml");
    assert_eq!(linked.state, ConfigDocumentState::Symlink);
    assert_eq!(linked.value, Some(json!({ "enabled": true })));

    assert!(matches!(
        read_document(rig.store.catalog(), rig.store.paths(), "tiling"),
        ConfigDocumentOutcome::UnknownFile { .. }
    ));
}

#[test]
fn open_refuses_names_outside_the_catalog() {
    let rig = Rig::new();
    assert_eq!(
        open_file(rig.store.catalog(), rig.store.paths(), "/etc/hosts"),
        OpenConfigFileOutcome::UnknownFile
    );
}

#[test]
fn templates_are_created_without_clobbering() {
    let rig = Rig::new();
    let id = stache_tiling::TilingConfig::ID;
    let path = create_from_template(rig.store.catalog(), rig.store.paths(), id).expect("create");
    assert!(rig.read("tiling.toml").starts_with("#:schema "));
    fs::write(&path, "enabled = true\n").expect("write");
    create_from_template(rig.store.catalog(), rig.store.paths(), id).expect("existing");
    assert_eq!(rig.read("tiling.toml"), "enabled = true\n");
}

#[test]
fn outcomes_are_typed_unions_on_the_wire() {
    let json = serde_json::to_value(ConfigEditOutcome::Stale { hash: None }).expect("encode");
    assert_eq!(json, json!({ "status": "stale", "hash": null }));
    let request: ConfigEditRequest = serde_json::from_value(json!({
        "file": "tiling.toml",
        "baseHash": null,
        "op": { "op": "remove", "path": ["gaps", "inner"] }
    }))
    .expect("decode");
    assert_eq!(request.op, ConfigEditOp::Remove {
        path: vec![key("gaps"), key("inner")]
    });
}
