use serde::{Deserialize, Serialize};

use crate::RevisionSetV1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionMismatch {
    Target,
    Dependencies,
}

pub fn compare_revisions(
    expected: &RevisionSetV1,
    current: &RevisionSetV1,
) -> Result<(), RevisionMismatch> {
    if expected.target != current.target {
        return Err(RevisionMismatch::Target);
    }
    if expected.dependencies != current.dependencies {
        return Err(RevisionMismatch::Dependencies);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn target_conflict_has_priority_over_dependency_drift() {
        let expected = RevisionSetV1 {
            target: 1,
            dependencies: BTreeMap::from([("bundle".to_owned(), 2)]),
        };
        let current = RevisionSetV1 {
            target: 3,
            dependencies: BTreeMap::new(),
        };
        assert_eq!(
            compare_revisions(&expected, &current),
            Err(RevisionMismatch::Target)
        );
    }
}
