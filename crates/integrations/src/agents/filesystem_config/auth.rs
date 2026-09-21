//! Privileged credential parsing; called only after selected source validation.
use super::*;

pub(super) fn read_settings_auth_field(
    path: &Path,
    process: &BTreeMap<String, Zeroizing<String>>,
    field: &str,
) -> Result<(Zeroizing<String>, u64), AgentFilesystemScanError> {
    let Some((bytes, metadata)) = read_validated_config_bytes(path)? else {
        return Err(AgentFilesystemScanError::SourceUnavailable);
    };
    let digest = CanonicalDigest::of_bytes(&bytes);
    let protected: ClaudeAuthSettingsSubset =
        serde_json::from_slice(&bytes).map_err(|_| AgentFilesystemScanError::InvalidConfig)?;
    let token = if field == "ANTHROPIC_API_KEY" {
        protected.api_key
    } else {
        protected.auth_token
    }
    .ok_or(AgentFilesystemScanError::SourceUnavailable)?;
    let token = resolve_explicit_environment_reference(token, process)?;
    Ok((token, metadata_revision(&metadata, &digest)))
}

#[derive(Default)]
struct ClaudeAuthSettingsSubset {
    auth_token: Option<Zeroizing<String>>,
    api_key: Option<Zeroizing<String>>,
}

impl<'de> Deserialize<'de> for ClaudeAuthSettingsSubset {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SettingsVisitor;
        impl<'de> Visitor<'de> for SettingsVisitor {
            type Value = ClaudeAuthSettingsSubset;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Claude settings object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut auth_token = None;
                let mut api_key = None;
                let mut seen_env = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "env" {
                        if seen_env {
                            return Err(serde::de::Error::duplicate_field("env"));
                        }
                        seen_env = true;
                        let environment = map.next_value::<ClaudeAuthEnvironmentSubset>()?;
                        auth_token = environment.auth_token;
                        api_key = environment.api_key;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(ClaudeAuthSettingsSubset {
                    auth_token,
                    api_key,
                })
            }
        }
        deserializer.deserialize_map(SettingsVisitor)
    }
}

#[derive(Default)]
struct ClaudeAuthEnvironmentSubset {
    auth_token: Option<Zeroizing<String>>,
    api_key: Option<Zeroizing<String>>,
}

impl<'de> Deserialize<'de> for ClaudeAuthEnvironmentSubset {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EnvironmentVisitor;
        impl<'de> Visitor<'de> for EnvironmentVisitor {
            type Value = ClaudeAuthEnvironmentSubset;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Claude environment object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut auth_token = None;
                let mut api_key = None;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "ANTHROPIC_AUTH_TOKEN" {
                        if auth_token.is_some() {
                            return Err(serde::de::Error::duplicate_field("ANTHROPIC_AUTH_TOKEN"));
                        }
                        auth_token = Some(Zeroizing::new(map.next_value()?));
                    } else if key == "ANTHROPIC_API_KEY" {
                        if api_key.is_some() {
                            return Err(serde::de::Error::duplicate_field("ANTHROPIC_API_KEY"));
                        }
                        api_key = Some(Zeroizing::new(map.next_value()?));
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(ClaudeAuthEnvironmentSubset {
                    auth_token,
                    api_key,
                })
            }
        }
        deserializer.deserialize_map(EnvironmentVisitor)
    }
}
