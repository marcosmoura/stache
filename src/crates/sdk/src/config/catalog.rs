//! Typed configuration types and the metadata derived from them.

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use schemars::{JsonSchema, Schema};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::diagnostics::{ConfigDiagnostic, ConfigFileId};
use crate::config::global::GlobalConfig;
use crate::config::{loader, schema, template};
use crate::error::ConfigError;
use crate::module::{ModuleConfig, ModuleId};

/// A decoded configuration together with the unknown keys it ignored.
pub struct DecodedConfig {
    value: Box<dyn Any + Send + Sync>,
    warnings: Vec<String>,
}

impl DecodedConfig {
    /// Splits the value from its warnings.
    #[must_use]
    pub fn into_parts(self) -> (Box<dyn Any + Send + Sync>, Vec<String>) {
        (self.value, self.warnings)
    }
}

/// Everything the catalog needs to describe one configuration type.
#[derive(Clone)]
struct ConfigKind {
    type_id: TypeId,
    default: Value,
    decode: fn(Value) -> Result<DecodedConfig, DecodeFailure>,
    validate: fn(&dyn Any) -> Result<(), ConfigError>,
    schema: fn() -> Schema,
}

/// A typed decode failure with an optional field path.
struct DecodeFailure {
    field: Option<String>,
    message: String,
}

fn decode_value<C: DeserializeOwned + Send + Sync + 'static>(
    value: Value,
) -> Result<DecodedConfig, DecodeFailure> {
    let mut track = serde_path_to_error::Track::new();
    let mut warnings = Vec::new();
    let result = serde_ignored::deserialize::<_, _, C>(
        serde_path_to_error::Deserializer::new(value, &mut track),
        |path| {
            let path = path.to_string();
            if path != "$schema" {
                warnings.push(path);
            }
        },
    );
    match result {
        Ok(config) => Ok(DecodedConfig {
            value: Box::new(config),
            warnings,
        }),
        Err(error) => {
            let field = track.path().to_string();
            Err(DecodeFailure {
                field: (!field.is_empty() && field != ".").then_some(field),
                message: error.to_string(),
            })
        }
    }
}

fn validate_value<C: ModuleConfig>(value: &dyn Any) -> Result<(), ConfigError> {
    value
        .downcast_ref::<C>()
        .ok_or_else(|| ConfigError::new("registered configuration type mismatch"))
        .and_then(ModuleConfig::validate)
}

/// The registered configuration types, keyed by exact module identifier.
///
/// Registration is the only place that knows a concrete configuration type;
/// every other operation is type-erased.
#[derive(Clone)]
pub struct ConfigCatalog {
    kinds: BTreeMap<ModuleId, ConfigKind>,
}

impl ConfigCatalog {
    /// Creates an empty catalog.
    #[must_use]
    pub const fn new() -> Self { Self { kinds: BTreeMap::new() } }

    /// Registers one configuration type under its exact identifier.
    ///
    /// The type's default is serialized once so templates and file generation
    /// cannot disagree with the type.
    ///
    /// # Errors
    /// Returns [`CatalogError`] for a duplicate identifier or a default value
    /// that cannot be serialized.
    pub fn register<C>(&mut self, id: ModuleId) -> Result<(), CatalogError>
    where C: ModuleConfig + Default + Serialize + DeserializeOwned + JsonSchema {
        if self.kinds.contains_key(&id) {
            return Err(CatalogError::Duplicate(id));
        }
        let default = serde_json::to_value(C::default())
            .map_err(|error| CatalogError::Serialize { id, message: error.to_string() })?;
        self.kinds.insert(id, ConfigKind {
            type_id: TypeId::of::<C>(),
            default,
            decode: decode_value::<C>,
            validate: validate_value::<C>,
            schema: || schemars::schema_for!(C),
        });
        Ok(())
    }

    /// Identifiers in file order: `global` first, then module IDs.
    pub fn ids(&self) -> impl Iterator<Item = ModuleId> + '_ {
        let global = self.kinds.contains_key(&GlobalConfig::ID).then_some(GlobalConfig::ID);
        global
            .into_iter()
            .chain(self.kinds.keys().copied().filter(|id| *id != GlobalConfig::ID))
    }

    /// Whether an identifier is registered.
    #[must_use]
    pub fn contains(&self, id: ModuleId) -> bool { self.kinds.contains_key(&id) }

    /// The registered type ID, used to reject a mismatched handle.
    pub(crate) fn type_id(&self, id: ModuleId) -> Option<TypeId> {
        self.kinds.get(&id).map(|kind| kind.type_id)
    }

    /// The file name for one registered identifier.
    #[must_use]
    pub fn file_name(&self, id: ModuleId) -> Option<String> {
        self.kinds.contains_key(&id).then(|| ConfigFileId::module(id).file_name())
    }

    /// The serialized default for one identifier.
    #[must_use]
    pub fn default_value(&self, id: ModuleId) -> Option<&Value> {
        self.kinds.get(&id).map(|kind| &kind.default)
    }

    /// The commented template for one identifier.
    #[must_use]
    pub fn template(&self, id: ModuleId) -> Option<String> {
        let kind = self.kinds.get(&id)?;
        let schema = (kind.schema)();
        Some(template::render(
            ConfigFileId::module(id),
            &kind.default,
            &schema,
        ))
    }

    /// The generated schema for one identifier.
    #[must_use]
    pub fn schema(&self, id: ModuleId) -> Option<Schema> {
        self.kinds.get(&id).map(|kind| (kind.schema)())
    }

    /// The deterministic schema file contents for one identifier.
    ///
    /// # Errors
    /// Returns a serialization error rather than emitting a partial schema.
    #[must_use]
    pub fn schema_json(&self, id: ModuleId) -> Option<Result<String, serde_json::Error>> {
        let mut schema = self.schema(id)?;
        schema.insert(
            "$id".to_owned(),
            Value::String(schema::schema_url(ConfigFileId::module(id))),
        );
        Some(schema::render(&schema))
    }

    /// Parses and deserializes one file into its registered type.
    ///
    /// Validation belongs to the registry, which records the diagnostic on the
    /// module snapshot. Use [`ConfigCatalog::inspect`] for an offline check.
    ///
    /// # Errors
    /// Returns a parse or type diagnostic naming the file.
    pub fn decode(
        &self,
        id: ModuleId,
        path: &Path,
        text: &str,
    ) -> Result<DecodedConfig, ConfigDiagnostic> {
        let file = ConfigFileId::module(id);
        let value = loader::parse_text(file, path, text)?;
        let kind = self.kinds.get(&id).ok_or_else(|| {
            ConfigDiagnostic::application(file, path, "configuration is not registered")
        })?;
        (kind.decode)(value).map_err(|failure| {
            ConfigDiagnostic::typed_message(file, path, failure.field, &failure.message)
        })
    }

    /// Decodes one file into `C`, rejecting a catalog/type mismatch.
    ///
    /// Returns the value and the unknown keys the decoder ignored.
    ///
    /// # Errors
    /// Returns a diagnostic for a missing entry, a type mismatch or invalid
    /// file contents.
    pub fn decode_typed<C: ModuleConfig + DeserializeOwned>(
        &self,
        id: ModuleId,
        path: &Path,
        text: &str,
    ) -> Result<(C, Vec<String>), ConfigDiagnostic> {
        let file = ConfigFileId::module(id);
        let (value, warnings) = self.decode(id, path, text)?.into_parts();
        value.downcast::<C>().map(|config| (*config, warnings)).map_err(|_| {
            ConfigDiagnostic::application(file, path, "registered configuration type mismatch")
        })
    }

    /// Parses, deserializes and validates one file without applying it.
    ///
    /// Returns the unknown keys the decoder ignored.
    ///
    /// # Errors
    /// Returns a parse, type or validation diagnostic.
    pub fn inspect(
        &self,
        id: ModuleId,
        path: &Path,
        text: &str,
    ) -> Result<Vec<String>, ConfigDiagnostic> {
        let file = ConfigFileId::module(id);
        let decoded = self.decode(id, path, text)?;
        let kind = self.kinds.get(&id).ok_or_else(|| {
            ConfigDiagnostic::application(file, path, "configuration is not registered")
        })?;
        (kind.validate)(decoded.value.as_ref())
            .map_err(|error| ConfigDiagnostic::validation(file, path, &error))?;
        Ok(decoded.warnings)
    }
}

impl Default for ConfigCatalog {
    fn default() -> Self { Self::new() }
}

/// A catalog registration failure.
#[derive(Debug)]
pub enum CatalogError {
    /// The identifier is already registered.
    Duplicate(ModuleId),
    /// The type's default value could not be serialized.
    Serialize {
        /// The identifier being registered.
        id: ModuleId,
        /// The serialization reason.
        message: String,
    },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(id) => write!(f, "configuration type `{id}` is already registered"),
            Self::Serialize { id, message } => {
                write!(
                    f,
                    "configuration type `{id}` has an unserializable default: {message}"
                )
            }
        }
    }
}

impl std::error::Error for CatalogError {}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default)]
    struct Sample {
        /// Whether the sample is enabled.
        enabled: bool,
    }

    impl ModuleConfig for Sample {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<Sample>(ModuleId::new("sample")).expect("first");
        assert!(matches!(
            catalog.register::<Sample>(ModuleId::new("sample")),
            Err(CatalogError::Duplicate(_))
        ));
    }

    #[test]
    fn global_is_listed_first_and_templates_carry_comments() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<Sample>(ModuleId::new("sample")).expect("sample");
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        let ids: Vec<&str> = catalog.ids().map(ModuleId::as_str).collect();
        assert_eq!(ids, ["global", "sample"]);
        let template = catalog.template(GlobalConfig::ID).expect("template");
        assert!(template.contains("\"$schema\""));
        assert!(template.contains("\"execOnStartup\""));
    }

    #[test]
    fn unknown_keys_are_warnings_but_the_file_stays_valid() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<Sample>(ModuleId::new("sample")).expect("sample");
        let text = r#"{
            "$schema": "./schemas/sample.schema.json",
            "enabled": true,
            "enable": true,
            "nested": { "typo": 1 }
        }"#;
        let warnings = catalog
            .inspect(ModuleId::new("sample"), Path::new("sample.jsonc"), text)
            .expect("the file is still valid");
        assert_eq!(warnings, ["enable", "nested"]);
    }

    #[test]
    fn unknown_keys_inside_a_known_section_keep_their_path() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        let warnings = catalog
            .inspect(
                GlobalConfig::ID,
                Path::new("global.jsonc"),
                "{ \"logging\": { \"levl\": \"debug\" } }",
            )
            .expect("the file is still valid");
        assert_eq!(warnings, ["logging.levl"]);
    }

    #[test]
    fn type_errors_keep_the_field_path() {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<Sample>(ModuleId::new("sample")).expect("sample");
        let diagnostic = catalog
            .inspect(
                ModuleId::new("sample"),
                Path::new("sample.jsonc"),
                "{ \"enabled\": \"yes\" }",
            )
            .expect_err("the wrong type is rejected");
        assert_eq!(diagnostic.kind, crate::config::DiagnosticKind::Type);
        assert_eq!(diagnostic.field.as_deref(), Some("enabled"));
    }
}
