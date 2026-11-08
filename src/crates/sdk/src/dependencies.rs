//! Pure validation of the `provides` / `requires` declarations.
//!
//! These functions never call a module. Registration uses them to reject
//! duplicate providers, self-dependencies and cycles before anything starts;
//! `start_all` uses the ordering function to reject missing providers and to
//! start prerequisites first.

use std::collections::HashMap;

use crate::error::RegistryError;
use crate::module::{ModuleDescriptor, ModuleId, ServiceId};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Unvisited,
    Visiting,
    Done,
}

/// Validates declarations that can be checked without requiring every provider
/// to be registered yet: duplicate providers, self-dependencies and cycles.
pub fn check_registration(descriptors: &[ModuleDescriptor]) -> Result<(), RegistryError> {
    let providers = provider_index(descriptors)?;
    detect_cycles(descriptors, &providers).map(|_| ())
}

/// Validates the complete graph and returns modules in dependency order
/// (providers before consumers, registration order among independent modules).
pub fn order(descriptors: &[ModuleDescriptor]) -> Result<Vec<ModuleId>, RegistryError> {
    let providers = provider_index(descriptors)?;
    for descriptor in descriptors {
        for &service in descriptor.requires {
            if !providers.contains_key(&service) {
                return Err(RegistryError::MissingProvider { module: descriptor.id, service });
            }
        }
    }
    detect_cycles(descriptors, &providers)
}

fn provider_index(
    descriptors: &[ModuleDescriptor],
) -> Result<HashMap<ServiceId, ModuleId>, RegistryError> {
    let mut providers = HashMap::new();
    for descriptor in descriptors {
        for &service in descriptor.provides {
            if let Some(&first) = providers.get(&service) {
                return Err(RegistryError::DuplicateProvider {
                    service,
                    first,
                    second: descriptor.id,
                });
            }
            providers.insert(service, descriptor.id);
        }
        for &service in descriptor.requires {
            if descriptor.provides.contains(&service) {
                return Err(RegistryError::SelfDependency { module: descriptor.id, service });
            }
        }
    }
    Ok(providers)
}

fn detect_cycles(
    descriptors: &[ModuleDescriptor],
    providers: &HashMap<ServiceId, ModuleId>,
) -> Result<Vec<ModuleId>, RegistryError> {
    let by_id: HashMap<ModuleId, &ModuleDescriptor> =
        descriptors.iter().map(|descriptor| (descriptor.id, descriptor)).collect();
    let mut marks: HashMap<ModuleId, Mark> =
        descriptors.iter().map(|descriptor| (descriptor.id, Mark::Unvisited)).collect();
    let mut stack = Vec::new();
    let mut ordered = Vec::new();
    for descriptor in descriptors {
        visit(
            descriptor.id,
            &by_id,
            providers,
            &mut marks,
            &mut stack,
            &mut ordered,
        )?;
    }
    Ok(ordered)
}

#[allow(clippy::too_many_arguments)]
fn visit(
    id: ModuleId,
    by_id: &HashMap<ModuleId, &ModuleDescriptor>,
    providers: &HashMap<ServiceId, ModuleId>,
    marks: &mut HashMap<ModuleId, Mark>,
    stack: &mut Vec<ModuleId>,
    ordered: &mut Vec<ModuleId>,
) -> Result<(), RegistryError> {
    match marks.get(&id).copied().unwrap_or(Mark::Unvisited) {
        Mark::Done => return Ok(()),
        Mark::Visiting => {
            let start = stack.iter().position(|entry| *entry == id).unwrap_or(0);
            let mut cycle = stack[start..].to_vec();
            cycle.push(id);
            return Err(RegistryError::CyclicDependency { cycle });
        }
        Mark::Unvisited => {}
    }
    marks.insert(id, Mark::Visiting);
    stack.push(id);
    if let Some(descriptor) = by_id.get(&id) {
        for &service in descriptor.requires {
            if let Some(&provider) = providers.get(&service) {
                visit(provider, by_id, providers, marks, stack, ordered)?;
            }
        }
    }
    stack.pop();
    marks.insert(id, Mark::Done);
    ordered.push(id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVICE_A: ServiceId = ServiceId::new("service.a");
    const SERVICE_B: ServiceId = ServiceId::new("service.b");

    static PROVIDES_A: [ServiceId; 1] = [SERVICE_A];
    static PROVIDES_B: [ServiceId; 1] = [SERVICE_B];
    static REQUIRES_A: [ServiceId; 1] = [SERVICE_A];
    static REQUIRES_B: [ServiceId; 1] = [SERVICE_B];

    fn descriptor(
        id: &'static str,
        provides: &'static [ServiceId],
        requires: &'static [ServiceId],
    ) -> ModuleDescriptor {
        ModuleDescriptor {
            id: ModuleId::new(id),
            provides,
            requires,
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    #[test]
    fn providers_start_before_consumers() {
        let descriptors = [
            descriptor("consumer", &[], &REQUIRES_A),
            descriptor("provider", &PROVIDES_A, &[]),
        ];
        let order = order(&descriptors).expect("valid graph");
        assert_eq!(order, vec![ModuleId::new("provider"), ModuleId::new("consumer")]);
    }

    #[test]
    fn independent_modules_keep_registration_order() {
        let descriptors = [
            descriptor("first", &[], &[]),
            descriptor("second", &[], &[]),
        ];
        let order = order(&descriptors).expect("valid graph");
        assert_eq!(order, vec![ModuleId::new("first"), ModuleId::new("second")]);
    }

    #[test]
    fn duplicate_providers_are_rejected() {
        let descriptors = [
            descriptor("first", &PROVIDES_A, &[]),
            descriptor("second", &PROVIDES_A, &[]),
        ];
        let error = check_registration(&descriptors).expect_err("duplicate provider");
        assert!(matches!(
            error,
            RegistryError::DuplicateProvider { first, second, .. }
                if first == ModuleId::new("first") && second == ModuleId::new("second")
        ));
    }

    #[test]
    fn self_dependencies_are_rejected() {
        let descriptors = [descriptor("selfish", &PROVIDES_A, &REQUIRES_A)];
        let error = check_registration(&descriptors).expect_err("self dependency");
        assert!(matches!(
            error,
            RegistryError::SelfDependency { module, service }
                if module == ModuleId::new("selfish") && service == SERVICE_A
        ));
    }

    #[test]
    fn missing_providers_are_rejected_before_startup() {
        let descriptors = [descriptor("consumer", &[], &REQUIRES_A)];
        let error = order(&descriptors).expect_err("missing provider");
        assert!(matches!(
            error,
            RegistryError::MissingProvider { module, service }
                if module == ModuleId::new("consumer") && service == SERVICE_A
        ));
    }

    #[test]
    fn cycles_are_rejected_with_the_path() {
        let descriptors = [
            descriptor("alpha", &PROVIDES_A, &REQUIRES_B),
            descriptor("beta", &PROVIDES_B, &REQUIRES_A),
        ];
        let error = order(&descriptors).expect_err("cycle");
        match error {
            RegistryError::CyclicDependency { cycle } => {
                assert_eq!(cycle.first(), cycle.last());
                assert!(cycle.contains(&ModuleId::new("alpha")));
                assert!(cycle.contains(&ModuleId::new("beta")));
            }
            other => panic!("expected a cycle, got {other}"),
        }
    }

    #[test]
    fn registration_ignores_providers_that_are_not_registered_yet() {
        let descriptors = [descriptor("consumer", &[], &REQUIRES_A)];
        assert!(check_registration(&descriptors).is_ok());
    }

    #[test]
    fn registration_rejects_a_cycle_created_by_a_later_module() {
        let descriptors = [
            descriptor("alpha", &PROVIDES_A, &REQUIRES_B),
            descriptor("beta", &PROVIDES_B, &REQUIRES_A),
        ];
        assert!(check_registration(&descriptors).is_err());
    }
}
