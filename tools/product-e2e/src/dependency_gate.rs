use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DependencyGateFinding {
    pub package: String,
    pub forbidden_dependency: String,
}

pub fn verify_dependency_metadata(metadata: &Value) -> Result<(), Vec<DependencyGateFinding>> {
    let product_packages = BTreeSet::from([
        "hiroute-domain",
        "hiroute-application-api",
        "hiroute-application",
        "hiroute-cli",
        "hiroute-daemon",
        "hiroute-local-storage",
        "hiroute-integrations",
        "hiroute-observation",
        "hiroute-product-e2e",
    ]);
    let allowed = BTreeMap::from([
        ("hiroute-domain", BTreeSet::new()),
        (
            "hiroute-application-api",
            BTreeSet::from(["hiroute-domain"]),
        ),
        (
            "hiroute-application",
            BTreeSet::from(["hiroute-application-api", "hiroute-domain"]),
        ),
        (
            "hiroute-cli",
            BTreeSet::from(["hiroute-application-api", "hiroute-integrations"]),
        ),
        (
            "hiroute-daemon",
            BTreeSet::from([
                "hiroute-application",
                "hiroute-application-api",
                "hiroute-domain",
                "hiroute-integrations",
                "hiroute-local-storage",
                "hiroute-observation",
            ]),
        ),
        ("hiroute-local-storage", BTreeSet::from(["hiroute-domain"])),
        (
            "hiroute-integrations",
            BTreeSet::from([
                "hiroute-application",
                "hiroute-application-api",
                "hiroute-domain",
            ]),
        ),
        ("hiroute-observation", BTreeSet::from(["hiroute-domain"])),
        (
            "hiroute-product-e2e",
            BTreeSet::from(["hiroute-application-api", "hiroute-domain"]),
        ),
    ]);

    let mut findings = Vec::new();
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for package in packages {
        let Some(name) = package.get("name").and_then(Value::as_str) else {
            continue;
        };
        let dependencies = package
            .get("dependencies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let product_dependencies = dependencies
            .iter()
            // Cargo metadata includes dev-only test wiring in this array. The frozen graph is a
            // production dependency-direction gate, so only normal dependencies participate.
            .filter(|dependency| dependency.get("kind").is_none_or(Value::is_null))
            .filter_map(|dependency| dependency.get("name").and_then(Value::as_str))
            .filter(|dependency| product_packages.contains(dependency))
            .collect::<BTreeSet<_>>();

        if let Some(package_allowed) = allowed.get(name) {
            for dependency in product_dependencies.difference(package_allowed) {
                findings.push(DependencyGateFinding {
                    package: name.to_owned(),
                    forbidden_dependency: (*dependency).to_owned(),
                });
            }
        } else if name == "hiroute-gateway" {
            let allowed = BTreeSet::from(["hiroute-domain"]);
            for dependency in product_dependencies.difference(&allowed) {
                findings.push(DependencyGateFinding {
                    package: name.to_owned(),
                    forbidden_dependency: (*dependency).to_owned(),
                });
            }
        } else if matches!(name, "hiroute-gateway-core" | "hiroute-e2e") {
            for dependency in product_dependencies {
                findings.push(DependencyGateFinding {
                    package: name.to_owned(),
                    forbidden_dependency: dependency.to_owned(),
                });
            }
        }
    }

    findings.sort_by(|left, right| {
        (&left.package, &left.forbidden_dependency)
            .cmp(&(&right.package, &right.forbidden_dependency))
    });
    if findings.is_empty() {
        Ok(())
    } else {
        Err(findings)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn dependency_gate_ignores_dev_only_wiring_but_rejects_the_same_runtime_edge() {
        let metadata = |kind: Value| {
            json!({
                "packages": [{
                    "name": "hiroute-cli",
                    "dependencies": [{ "name": "hiroute-daemon", "kind": kind }]
                }]
            })
        };

        verify_dependency_metadata(&metadata(json!("dev"))).unwrap();
        assert_eq!(
            verify_dependency_metadata(&metadata(Value::Null)).unwrap_err(),
            vec![DependencyGateFinding {
                package: "hiroute-cli".to_owned(),
                forbidden_dependency: "hiroute-daemon".to_owned(),
            }]
        );
    }
}
