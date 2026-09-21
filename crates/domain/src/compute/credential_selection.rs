use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CredentialRefV1, GatewayAuthenticationSemanticsV1};

/// Explicit credential closure carried by a compiled compute candidate.
///
/// An empty credential list is ambiguous: it could mean a valid unauthenticated target or an
/// incompletely materialized authenticated target. This tagged value keeps those states distinct
/// before any credential resolver or lease is consulted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputeCredentialSelectionV2 {
    NoCredential,
    Credential { credential_ref: CredentialRefV1 },
}

impl ComputeCredentialSelectionV2 {
    pub fn validate_for(
        &self,
        authentication: &GatewayAuthenticationSemanticsV1,
    ) -> Result<(), ComputeCredentialSelectionError> {
        match (self, authentication) {
            (Self::NoCredential, GatewayAuthenticationSemanticsV1::None) => Ok(()),
            (
                Self::Credential { credential_ref },
                GatewayAuthenticationSemanticsV1::Bearer
                | GatewayAuthenticationSemanticsV1::ApiKeyHeader { .. },
            ) if credential_ref.generation() > 0 => Ok(()),
            (Self::Credential { .. }, GatewayAuthenticationSemanticsV1::None)
            | (Self::NoCredential, GatewayAuthenticationSemanticsV1::Bearer)
            | (Self::NoCredential, GatewayAuthenticationSemanticsV1::ApiKeyHeader { .. }) => {
                Err(ComputeCredentialSelectionError::AuthenticationMismatch)
            }
            (Self::Credential { .. }, _) => {
                Err(ComputeCredentialSelectionError::InvalidCredentialGeneration)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ComputeCredentialSelectionError {
    #[error("credential selection does not match authentication semantics")]
    AuthenticationMismatch,
    #[error("credential selection has an invalid generation")]
    InvalidCredentialGeneration,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(generation: u64) -> CredentialRefV1 {
        CredentialRefV1::new(
            "credential/native-key",
            "compute/source",
            "source/native",
            "model-inference",
            ["connection-option/native".to_owned()],
            generation,
        )
        .unwrap()
    }

    #[test]
    fn unauthenticated_candidates_require_the_explicit_no_credential_branch() {
        assert_eq!(
            ComputeCredentialSelectionV2::NoCredential
                .validate_for(&GatewayAuthenticationSemanticsV1::None),
            Ok(())
        );
        assert_eq!(
            ComputeCredentialSelectionV2::Credential {
                credential_ref: credential(1),
            }
            .validate_for(&GatewayAuthenticationSemanticsV1::None),
            Err(ComputeCredentialSelectionError::AuthenticationMismatch)
        );
    }

    #[test]
    fn authenticated_candidates_cannot_use_an_empty_credential_closure() {
        for authentication in [
            GatewayAuthenticationSemanticsV1::Bearer,
            GatewayAuthenticationSemanticsV1::ApiKeyHeader {
                header: "x-api-key".to_owned(),
            },
        ] {
            assert_eq!(
                ComputeCredentialSelectionV2::NoCredential.validate_for(&authentication),
                Err(ComputeCredentialSelectionError::AuthenticationMismatch)
            );
            assert_eq!(
                ComputeCredentialSelectionV2::Credential {
                    credential_ref: credential(1),
                }
                .validate_for(&authentication),
                Ok(())
            );
        }
    }
}
