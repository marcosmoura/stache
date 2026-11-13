//! End-to-end settings-editor scenario over the real host runner, the real
//! configuration store and the real directory watcher on a temp directory.
//!
//! Only the modules are probes: they accept `tiling.toml` and
//! `keybindings.toml` with the production configuration types and do nothing
//! else, so the scenario observes the accepted revision without native work.
//! Every edit goes through the command layer (`HostCommand::ConfigEdit`, the
//! same runner path `config_edit` uses); every external save goes through the
//! watcher and `apply_config`. Each step asserts the exact bytes, the accepted
//! revision and exactly one `config-changed`.

use std::marker::PhantomData;
use std::sync::mpsc::Receiver;

use serde_json::json;
use stache_sdk::config::{ConfigEditOp, PathSegment, content_hash};
use stache_sdk::{ModuleDescriptor, ModuleResult};

use super::tests::{AllowAll, runner_over};
use super::*;
use crate::config_documents::{
    ConfigDocument, ConfigDocumentOutcome, ConfigDocumentState, ConfigEditRequest,
    ConfigReloadView, read_document,
};

pub(super) const TILING: &str = r#"#:schema https://example.invalid/tiling.schema.json

# Tiling is opt-in.
enabled = false

# Gaps between windows and from the display edges.
[gaps]
inner = 10 # between windows
outer = 14

# The terminal workspace.
[[workspaces]]
name = "terminal"
layout = "monocle"
screen = "main"
rules = [{ app_id = "com.mitchellh.ghostty" }]

# The coding workspace.
[[workspaces]]
name = "coding"
layout = "dwindle"
screen = "main"
rules = [
  { app_id = "dev.zed.Zed" }, # editor
  { app_id = "com.apple.finder" },
]
"#;

const KEYBINDINGS: &str = r#"#:schema https://example.invalid/keybindings.schema.json

enabled = false

[bindings]
# Quit the frontmost app.
"Cmd+Q" = "/bin/sh quit" # keep
"Cmd+Shift+E" = ["/bin/sh one", "/bin/sh two"]
"#;

/// A module that accepts one configuration type and does nothing else.
struct Probe<C> {
    id: ModuleId,
    config: PhantomData<fn() -> C>,
}

impl<C: ModuleConfig> Module for Probe<C> {
    type Config = C;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: self.id,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    fn configure(&mut self, _: &C, _: &stache_sdk::ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn start(&mut self, _: &stache_sdk::ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn reconfigure(&mut self, _: &C, _: &stache_sdk::ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn stop(&mut self, _: &stache_sdk::ModuleContext) -> ModuleResult<()> { Ok(()) }
}

struct ProbeFactory<C> {
    id: ModuleId,
    config: PhantomData<fn() -> C>,
}

impl<C> ModuleFactory for ProbeFactory<C>
where C: ModuleConfig + Default + DeserializeOwned
{
    fn id(&self) -> ModuleId { self.id }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        register_module(registry, store, Probe::<C> {
            id: self.id,
            config: PhantomData,
        })
    }
}

fn probe<C: ModuleConfig + Default + DeserializeOwned>(id: ModuleId) -> Arc<dyn ModuleFactory> {
    Arc::new(ProbeFactory::<C> { id, config: PhantomData })
}

#[derive(Default)]
pub(super) struct RecordingUi {
    changes: Mutex<Vec<Vec<String>>>,
}

impl HostUi for RecordingUi {
    fn status(&self, _: &HostStatus) {}

    fn config_changed(&self, files: &[String]) {
        self.changes.lock().unwrap_or_else(PoisonError::into_inner).push(files.to_vec());
    }
}

impl RecordingUi {
    pub(super) fn take(&self) -> Vec<Vec<String>> {
        std::mem::take(&mut *self.changes.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

pub(super) struct Scenario {
    pub(super) runner: Runner,
    pub(super) receiver: Receiver<HostCommand>,
    pub(super) ui: Arc<RecordingUi>,
    _root: tempfile::TempDir,
}

fn key(name: &str) -> PathSegment { PathSegment::Key(name.to_owned()) }

impl Scenario {
    pub(super) fn new() -> Self {
        let root = tempfile::tempdir().expect("root");
        let config = root.path().join("config");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(config.join("tiling.toml"), TILING).expect("tiling");
        std::fs::write(config.join("keybindings.toml"), KEYBINDINGS).expect("keybindings");
        let mut catalog = ConfigCatalog::new();
        catalog
            .register::<stache_sdk::GlobalConfig>(stache_sdk::GlobalConfig::ID)
            .expect("global");
        catalog.register::<TilingConfig>(TilingConfig::ID).expect("tiling");
        catalog
            .register::<KeybindingsConfig>(KeybindingsConfig::ID)
            .expect("keybindings");
        let mut factories = ModuleFactories::new();
        factories.entries.push(probe::<TilingConfig>(TilingConfig::ID));
        factories.entries.push(probe::<KeybindingsConfig>(KeybindingsConfig::ID));
        let (mut runner, receiver) =
            runner_over(root.path(), factories, catalog, Arc::new(AllowAll), None);
        let ui = Arc::new(RecordingUi::default());
        runner.ui = ui.clone();
        runner.start().expect("start");
        let mut scenario = Self {
            runner,
            receiver,
            ui,
            _root: root,
        };
        scenario.settle();
        scenario.ui.take();
        scenario
    }

    pub(super) fn path(&self, file: &str) -> std::path::PathBuf {
        self.runner.store.paths().directory().join(file)
    }

    pub(super) fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.path(file)).expect("read")
    }

    fn document(&self, file: &str) -> ConfigDocument {
        match read_document(self.runner.store.catalog(), self.runner.store.paths(), file) {
            ConfigDocumentOutcome::Document { document } => *document,
            ConfigDocumentOutcome::UnknownFile { .. } => panic!("unknown"),
        }
    }

    pub(super) fn revision(&self, id: &str) -> u64 {
        self.runner
            .registry
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id.as_str() == id)
            .expect("module")
            .revision
    }

    /// Dispatches watcher batches until the directory has been quiet for well
    /// over the debounce window.
    pub(super) fn settle(&mut self) {
        let mut quiet_since = Instant::now();
        while quiet_since.elapsed() < Duration::from_millis(900) {
            match self.receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(HostCommand::ConfigChanges(changes)) => {
                    self.runner.apply_config(&changes);
                    quiet_since = Instant::now();
                }
                Ok(HostCommand::WatcherFailed(message)) => panic!("watcher failed: {message}"),
                Ok(_) | Err(_) => {}
            }
        }
    }

    /// Sends one edit through the command layer with the document's hash.
    fn edit(&mut self, file: &str, op: ConfigEditOp) -> ConfigEditOutcome {
        let base_hash = self.document(file).hash;
        self.edit_with(file, base_hash, op)
    }

    fn edit_with(
        &mut self,
        file: &str,
        base_hash: Option<String>,
        op: ConfigEditOp,
    ) -> ConfigEditOutcome {
        let request = ConfigEditRequest {
            file: file.to_owned(),
            base_hash,
            op,
        };
        self.runner.config_edit(&request)
    }

    /// One committed edit: exact bytes, one accepted revision step, exactly
    /// one `config-changed` even after the watcher reports the write.
    fn step(&mut self, file: &str, module: &str, op: ConfigEditOp, expected: &str) {
        let before = self.revision(module);
        let outcome = self.edit(file, op);
        let ConfigEditOutcome::Committed { hash, reload } = outcome else {
            panic!("expected a commit: {outcome:?}");
        };
        assert_eq!(self.read(file), expected);
        assert_eq!(hash, content_hash(expected.as_bytes()));
        assert_eq!(reload, ConfigReloadView::Applied {
            revision: (before + 1).to_string()
        });
        self.settle();
        assert_eq!(
            self.revision(module),
            before + 1,
            "the watcher echo reloads nothing"
        );
        assert_eq!(
            self.ui.take(),
            vec![vec![file.to_owned()]],
            "exactly one config-changed"
        );
    }

    /// One external save through the watcher.
    fn external(&mut self, file: &str, text: Option<&str>) -> Vec<Vec<String>> {
        match text {
            Some(text) => std::fs::write(self.path(file), text).expect("write"),
            None => std::fs::remove_file(self.path(file)).expect("delete"),
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.ui.changes.lock().unwrap_or_else(PoisonError::into_inner).is_empty() {
            assert!(Instant::now() < deadline, "the watcher never reported {file}");
            if let Ok(HostCommand::ConfigChanges(changes)) =
                self.receiver.recv_timeout(Duration::from_millis(50))
            {
                self.runner.apply_config(&changes);
            }
        }
        self.settle();
        self.ui.take()
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario, step by step.
fn editing_tiling_and_keybindings_end_to_end() {
    let mut scenario = Scenario::new();

    // Change a tiling gap.
    let mut tiling = TILING.replacen("inner = 10", "inner = 8", 1);
    scenario.step(
        "tiling.toml",
        "tiling",
        ConfigEditOp::Set {
            path: vec![key("gaps"), key("inner")],
            value: json!(8),
        },
        &tiling,
    );

    // Add a workspace at the end.
    tiling.push_str(
        "\n[[workspaces]]\nlayout = \"monocle\"\nname = \"browser\"\nscreen = \"secondary\"\n",
    );
    scenario.step(
        "tiling.toml",
        "tiling",
        ConfigEditOp::Insert {
            path: vec![key("workspaces")],
            index: 2,
            value: json!({ "name": "browser", "layout": "monocle", "screen": "secondary" }),
        },
        &tiling,
    );

    // Reorder the first two workspaces; each keeps its comment.
    let terminal_start = tiling.find("# The terminal workspace.").expect("terminal");
    let coding_start = tiling.find("# The coding workspace.").expect("coding");
    let browser_start = tiling.find("[[workspaces]]\nlayout").expect("browser");
    let terminal = &tiling[terminal_start..coding_start];
    let coding = &tiling[coding_start..browser_start - 1];
    let reordered = format!(
        "{}{}\n{}\n{}",
        &tiling[..terminal_start],
        coding,
        terminal.trim_end_matches('\n'),
        &tiling[browser_start - 1..],
    );
    scenario.step(
        "tiling.toml",
        "tiling",
        ConfigEditOp::Move {
            path: vec![key("workspaces")],
            from: 1,
            to: 0,
        },
        &reordered,
    );
    let tiling = reordered;

    // Rename a keybinding; its comment and value stay.
    let keybindings = KEYBINDINGS.replacen("\"Cmd+Shift+E\"", "\"Cmd+Shift+R\"", 1);
    scenario.step(
        "keybindings.toml",
        "keybindings",
        ConfigEditOp::Rename {
            path: vec![key("bindings")],
            from: "Cmd+Shift+E".to_owned(),
            to: "Cmd+Shift+R".to_owned(),
        },
        &keybindings,
    );

    // Reset a key to its default.
    let tiling_reset = tiling.replacen("outer = 14\n", "", 1);
    scenario.step(
        "tiling.toml",
        "tiling",
        ConfigEditOp::Remove {
            path: vec![key("gaps"), key("outer")],
        },
        &tiling_reset,
    );

    // External saves: valid, invalid, valid again, then a delete.
    let old_hash = scenario.document("tiling.toml").hash;
    let revision = scenario.revision("tiling");
    let external = tiling_reset.replacen("inner = 8", "inner = 6", 1);
    assert_eq!(scenario.external("tiling.toml", Some(&external)), vec![vec![
        "tiling.toml".to_owned()
    ]]);
    assert_eq!(scenario.revision("tiling"), revision + 1);
    assert_eq!(
        scenario.document("tiling.toml").state,
        ConfigDocumentState::Valid
    );
    let stale = scenario.edit_with("tiling.toml", old_hash, ConfigEditOp::Set {
        path: vec![key("gaps"), key("inner")],
        value: json!(2),
    });
    assert_eq!(stale, ConfigEditOutcome::Stale {
        hash: Some(content_hash(external.as_bytes()))
    });
    assert_eq!(
        scenario.read("tiling.toml"),
        external,
        "a stale edit writes nothing"
    );
    assert!(scenario.ui.take().is_empty(), "a refused edit announces nothing");

    assert_eq!(scenario.external("tiling.toml", Some("enabled = \n")), vec![
        vec!["tiling.toml".to_owned()]
    ]);
    let invalid = scenario.document("tiling.toml");
    assert_eq!(invalid.state, ConfigDocumentState::Invalid);
    assert!(invalid.diagnostic.is_some());
    assert_eq!(
        scenario.revision("tiling"),
        revision + 1,
        "the last good config stays"
    );
    let refused = scenario.edit("tiling.toml", ConfigEditOp::Remove {
        path: vec![key("enabled")],
    });
    assert!(
        matches!(refused, ConfigEditOutcome::Refused { .. }),
        "{refused:?}"
    );

    assert_eq!(scenario.external("tiling.toml", Some(&external)), vec![vec![
        "tiling.toml".to_owned()
    ]]);
    assert_eq!(
        scenario.document("tiling.toml").state,
        ConfigDocumentState::Valid
    );

    assert_eq!(scenario.external("tiling.toml", None), vec![vec![
        "tiling.toml".to_owned()
    ]]);
    assert_eq!(
        scenario.document("tiling.toml").state,
        ConfigDocumentState::Missing
    );
    scenario.runner.finish(true);
}
