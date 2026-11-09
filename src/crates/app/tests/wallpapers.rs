//! Wallpapers factory integration with fake native and disposable paths.
//!
//! Every test uses a temporary home, configuration directory and cache. The
//! fake desktop records calls; no test constructs the real setter, reads the
//! user's configuration or starts a real coordinator for a populated pool.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::RgbImage;
use stache_app::host::{FactoryContext, ModuleFactories, ModuleFactory};
use stache_app::wallpapers::WallpapersFactory;
use stache_macos::{
    AssertionOwner, DisplayEntry, DisplayFrame, DisplayId, DisplaySnapshot, FakeDesktop,
};
use stache_sdk::ipc::IpcHandler;
use stache_sdk::{
    AppEnvironment, AppPaths, ConfigPaths, ConfigStore, DiscardLogger, EventBus, LogFilter,
    MainThreadExecutor, ModuleStatus, Permission, PermissionError, PermissionProvider,
    PermissionStatus, Registry, ReloadOutcome, ThreadError, UnavailableWindows,
};
use stache_wallpapers::{
    RealProcessor, ServiceConfig, SourceContext, SystemRandom, SystemRuntimeClock, WallpaperScreen,
    WallpaperSelection, WallpapersConfig, WallpapersOutcomeCause, WallpapersOutcomeStatus,
    WallpapersService, WallpapersSetOutcome, WallpapersSetParams,
};

struct InlineMain;

impl MainThreadExecutor for InlineMain {
    fn is_main_thread(&self) -> bool { true }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        job();
        Ok(())
    }
}

struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

struct Fixture {
    root: tempfile::TempDir,
    app: AppPaths,
    config: ConfigPaths,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let app = AppPaths::resolve(&AppEnvironment {
            home: Some(root.path().to_path_buf()),
            temporary: Some(root.path().to_path_buf()),
        })
        .expect("identity paths");
        let config = ConfigPaths::new(root.path().join("config"));
        fs::create_dir_all(config.directory()).expect("configuration directory");
        Self { root, app, config }
    }

    fn write(&self, file: &str, text: &str) {
        fs::write(self.config.directory().join(file), text).expect("configuration file");
    }

    fn image(&self, name: &str) -> PathBuf {
        let path = self.config.directory().join(name);
        let pixels = RgbImage::from_fn(8, 8, |x, y| {
            #[allow(clippy::cast_possible_truncation)]
            image::Rgb([(x * 32) as u8, (y * 32) as u8, 96])
        });
        let file = fs::File::create(&path).expect("image file");
        pixels
            .write_with_encoder(image::codecs::png::PngEncoder::new(file))
            .expect("encode png");
        path
    }

    fn registry() -> Arc<Registry> {
        Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(InlineMain),
            Arc::new(AllowAll),
            Arc::new(UnavailableWindows),
        ))
    }

    fn store(&self, registry: &Arc<Registry>) -> Arc<ConfigStore> {
        Arc::new(ConfigStore::new(
            Arc::clone(registry),
            stache_app::catalog::catalog().expect("catalog"),
            self.config.clone(),
            Arc::new(LogFilter::default()),
        ))
    }

    fn service(&self, desktop: &FakeDesktop) -> Arc<WallpapersService> {
        WallpapersService::new(ServiceConfig {
            paths: self.app.clone(),
            source: SourceContext::new(
                self.config.directory(),
                Some(self.root.path().to_path_buf()),
            ),
            backend: Arc::new(desktop.clone()),
            clock: Arc::new(SystemRuntimeClock),
            random: Arc::new(SystemRandom),
            processor: Arc::new(RealProcessor),
        })
        .expect("wallpaper service")
    }
}

fn snapshot() -> DisplaySnapshot {
    DisplaySnapshot::new(vec![DisplayEntry {
        id: DisplayId::new(1),
        frame: DisplayFrame { width: 1600.0, height: 900.0 },
        scale: 1.0,
    }])
}

fn file_params(fixture: &Fixture, image: &Path) -> WallpapersSetParams {
    WallpapersSetParams {
        config_directory: fixture.config.directory().display().to_string(),
        selection: WallpaperSelection::File {
            path: image.display().to_string(),
        },
        screen: WallpaperScreen::All,
    }
}

fn handle(
    factory: &WallpapersFactory,
    registry: &Arc<Registry>,
    store: &Arc<ConfigStore>,
) -> Vec<(String, Arc<dyn IpcHandler>)> {
    let context = FactoryContext::new(Arc::clone(registry), Arc::clone(store));
    factory
        .ipc_handlers(&context)
        .into_iter()
        .map(|(name, handler)| (name.to_owned(), handler))
        .collect()
}

fn outcome(handler: &Arc<dyn IpcHandler>, params: &WallpapersSetParams) -> WallpapersSetOutcome {
    let value = handler
        .handle("set", serde_json::to_value(params).expect("params"))
        .expect("handled request");
    serde_json::from_value(value).expect("typed outcome")
}

#[test]
fn registration_exposes_the_set_route_and_status_diagnostics() {
    let fixture = Fixture::new();
    let image = fixture.image("wall.png");
    fixture.write("wallpapers.toml", "enabled = false\nlist = [\"wall.png\"]\n");
    let registry = Fixture::registry();
    let store = fixture.store(&registry);
    let desktop = FakeDesktop::with_snapshot(snapshot());
    let service = fixture.service(&desktop);
    let factory = WallpapersFactory::with_service(service);

    let control = factory.register(&registry, &store).expect("register");
    assert_eq!(control.id(), WallpapersConfig::ID);
    let published = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.id == WallpapersConfig::ID)
        .expect("wallpapers snapshot");
    assert_eq!(published.status, ModuleStatus::Disabled);
    assert_eq!(published.revision, 1);
    assert!(published.diagnostic.is_none());

    let handlers = handle(&factory, &registry, &store);
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].0, "wallpapers");
    let result = outcome(&handlers[0].1, &file_params(&fixture, &image));
    assert_eq!(result.status, WallpapersOutcomeStatus::FullSuccess);
    assert_eq!(desktop.calls().len(), 1, "exactly one native set call");
    assert!(
        desktop.calls()[0].image.starts_with(fixture.app.cache_directory()),
        "{:?}",
        desktop.calls()[0].image
    );
}

#[test]
fn an_invalid_initial_file_is_diagnosed_and_has_no_accepted_configuration() {
    let fixture = Fixture::new();
    fixture.write("wallpapers.toml", "enabled = ");
    let registry = Fixture::registry();
    let store = fixture.store(&registry);
    let desktop = FakeDesktop::with_snapshot(snapshot());
    let factory = WallpapersFactory::with_service(fixture.service(&desktop));

    factory.register(&registry, &store).expect("register rejected");
    let context = FactoryContext::new(Arc::clone(&registry), Arc::clone(&store));
    let diagnostic = context
        .diagnostic(WallpapersConfig::ID)
        .expect("the held rejection is visible to the factory");
    assert!(diagnostic.to_string().contains("wallpapers"), "{diagnostic}");
    let published = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.id == WallpapersConfig::ID)
        .expect("wallpapers snapshot");
    assert_eq!(published.status, ModuleStatus::Stopped);
    assert_eq!(published.revision, 0);
    assert!(published.diagnostic.is_some());

    let handlers = handle(&factory, &registry, &store);
    let params = WallpapersSetParams {
        config_directory: fixture.config.directory().display().to_string(),
        selection: WallpaperSelection::Random,
        screen: WallpaperScreen::All,
    };
    let result = outcome(&handlers[0].1, &params);
    assert_eq!(result.status, WallpapersOutcomeStatus::Failure);
    assert_eq!(result.cause, Some(WallpapersOutcomeCause::NoAcceptedConfig));
    assert!(desktop.calls().is_empty());
}

#[test]
fn the_installed_watch_observes_accepted_revisions() {
    let fixture = Fixture::new();
    fixture.image("a.png");
    fixture.image("b.png");
    fixture.write("wallpapers.toml", "enabled = false\nlist = [\"a.png\"]\n");
    let registry = Fixture::registry();
    let store = fixture.store(&registry);
    let desktop = FakeDesktop::with_snapshot(snapshot());
    let factory = WallpapersFactory::with_service(fixture.service(&desktop));

    factory.register(&registry, &store).expect("register");
    let handlers = handle(&factory, &registry, &store);
    let before = outcome(&handlers[0].1, &WallpapersSetParams {
        config_directory: fixture.config.directory().display().to_string(),
        selection: WallpaperSelection::Collection { index: 1 },
        screen: WallpaperScreen::All,
    });
    assert_eq!(before.status, WallpapersOutcomeStatus::Failure);
    assert!(
        matches!(
            before.cause,
            Some(WallpapersOutcomeCause::InvalidSelection { .. })
        ),
        "{before:?}"
    );

    fixture.write(
        "wallpapers.toml",
        "enabled = false\nlist = [\"a.png\", \"b.png\"]\n",
    );
    assert_eq!(
        store.reload_one(WallpapersConfig::ID).expect("reload"),
        ReloadOutcome::Applied { revision: 2 }
    );

    let after = outcome(&handlers[0].1, &WallpapersSetParams {
        config_directory: fixture.config.directory().display().to_string(),
        selection: WallpaperSelection::Collection { index: 1 },
        screen: WallpaperScreen::All,
    });
    assert_eq!(after.status, WallpapersOutcomeStatus::FullSuccess);
    assert_eq!(desktop.calls().len(), 1);
    assert!(desktop.calls()[0].image.starts_with(fixture.app.cache_directory()));
}

#[test]
fn rejected_and_deleted_reloads_keep_the_last_accepted_pool_for_explicit_requests() {
    let fixture = Fixture::new();
    fixture.image("a.png");
    fixture.image("b.png");
    fixture.write(
        "wallpapers.toml",
        "enabled = false\nlist = [\"a.png\", \"b.png\"]\n",
    );
    let registry = Fixture::registry();
    let store = fixture.store(&registry);
    let desktop = FakeDesktop::with_snapshot(snapshot());
    let factory = WallpapersFactory::with_service(fixture.service(&desktop));
    factory.register(&registry, &store).expect("register");
    let handlers = handle(&factory, &registry, &store);
    let params = WallpapersSetParams {
        config_directory: fixture.config.directory().display().to_string(),
        selection: WallpaperSelection::Collection { index: 1 },
        screen: WallpaperScreen::All,
    };

    fixture.write("wallpapers.toml", "enabled = ");
    assert!(!matches!(
        store.reload_one(WallpapersConfig::ID).expect("rejected reload"),
        ReloadOutcome::Applied { .. }
    ));
    let rejected = outcome(&handlers[0].1, &params);
    assert_eq!(rejected.status, WallpapersOutcomeStatus::FullSuccess);

    fs::remove_file(fixture.config.directory().join("wallpapers.toml")).expect("delete");
    assert!(!matches!(
        store.reload_one(WallpapersConfig::ID).expect("deleted reload"),
        ReloadOutcome::Applied { .. }
    ));
    let deleted = outcome(&handlers[0].1, &params);
    assert_eq!(deleted.status, WallpapersOutcomeStatus::FullSuccess);
    assert_eq!(desktop.calls().len(), 2);
}

#[test]
fn a_missing_configuration_registers_disabled_without_a_coordinator() {
    let fixture = Fixture::new();
    let registry = Fixture::registry();
    let store = fixture.store(&registry);
    let desktop = FakeDesktop::with_snapshot(snapshot());
    let service = fixture.service(&desktop);
    let factory = WallpapersFactory::with_service(Arc::clone(&service));

    factory.register(&registry, &store).expect("register");
    let published = registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.id == WallpapersConfig::ID)
        .expect("wallpapers snapshot");
    assert_eq!(published.status, ModuleStatus::Disabled);
    assert!(!published.enabled);
    assert!(!store.paths().module_file(WallpapersConfig::ID).exists());

    let status = service.status();
    assert!(!status.closed);
    assert!(!status.coordinator_running, "registration is inert");
    assert!(!status.executing);
    assert!(!status.queued);
    assert!(status.session.is_none());
}

#[test]
fn the_production_composition_registers_wallpapers_by_id() {
    let fixture = Fixture::new();
    let factories = ModuleFactories::production(Arc::new(InlineMain), AssertionOwner::system())
        .with(
            WallpapersFactory::new(
                fixture.app.clone(),
                fixture.config.directory(),
                Some(fixture.root.path().to_path_buf()),
                Arc::new(InlineMain),
            )
            .expect("inert wallpapers factory"),
        );
    let ids: Vec<&str> = factories.entries().iter().map(|factory| factory.id().as_str()).collect();
    assert_eq!(ids, [
        "proxyAudio",
        "notunes",
        "keepAwake",
        "menuAnywhere",
        "wallpapers"
    ]);
}
