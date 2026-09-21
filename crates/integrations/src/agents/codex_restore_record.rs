//! Private, bounded encoding stored only in the artifact store's encrypted sidecar.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: String,
    fields: Vec<Field>,
    created_tables: Vec<Vec<String>>,
    #[serde(default)]
    managed_aliases: Vec<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Field {
    path: Vec<String>,
    before: Option<String>,
    after: CanonicalDigest,
    #[serde(default)]
    passive_model: bool,
}
impl Drop for Field {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.before.zeroize();
    }
}
impl CodexNativeRestore {
    pub fn encode_protected(&self) -> Result<Zeroizing<Vec<u8>>, CodexNativeError> {
        let record = Record {
            schema: "hiroute.codex-field-restore/v1".into(),
            fields: self
                .fields
                .iter()
                .map(|field| Field {
                    path: field.path.clone(),
                    before: field.before.as_ref().map(ToString::to_string),
                    after: field.after.clone(),
                    passive_model: field.passive_model,
                })
                .collect(),
            created_tables: self.created_tables.clone(),
            managed_aliases: self.managed_aliases.clone(),
        };
        let encoded =
            Zeroizing::new(serde_json::to_vec(&record).map_err(|_| CodexNativeError::InvalidToml)?);
        if encoded.len() > MAX_BYTES {
            return Err(CodexNativeError::InvalidToml);
        }
        Ok(encoded)
    }

    pub fn decode_protected(bytes: &[u8]) -> Result<Self, CodexNativeError> {
        if bytes.len() > MAX_BYTES {
            return Err(CodexNativeError::InvalidToml);
        }
        let record: Record =
            serde_json::from_slice(bytes).map_err(|_| CodexNativeError::InvalidToml)?;
        if record.schema != "hiroute.codex-field-restore/v1"
            || !(7..=9).contains(&record.fields.len())
            || record.created_tables.len() > 3
            || record.managed_aliases.len() > 128
            || record
                .managed_aliases
                .iter()
                .any(|alias| ModelAlias::parse(alias.clone()).is_err())
            || record
                .managed_aliases
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != record.managed_aliases.len()
        {
            return Err(CodexNativeError::UnsupportedLayout);
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut fields = Vec::new();
        for mut field in record.fields {
            let valid = match field.path.as_slice() {
                [key] => matches!(
                    key.as_str(),
                    "model" | "model_provider" | "model_catalog_json"
                ),
                [parent, profile, key] if parent == "profiles" => {
                    identifier(profile)
                        && matches!(
                            key.as_str(),
                            "model" | "model_provider" | "model_catalog_json"
                        )
                }
                [parent, provider, key] => {
                    parent == "model_providers"
                        && provider == "hiroute"
                        && identifier(provider)
                        && matches!(
                            key.as_str(),
                            "name"
                                | "base_url"
                                | "wire_api"
                                | "requires_openai_auth"
                                | "supports_websockets"
                        )
                }
                [parent, provider, headers, key] => {
                    parent == "model_providers"
                        && provider == "hiroute"
                        && headers == "http_headers"
                        && key == "X-HiRoute-Token"
                }
                _ => false,
            };
            if !valid
                || (field.passive_model && field.path.last().map(String::as_str) != Some("model"))
                || !seen.insert(field.path.clone())
            {
                return Err(CodexNativeError::UnsupportedLayout);
            }
            let before = field
                .before
                .take()
                .map(|value| {
                    let value = Zeroizing::new(value);
                    let text = Zeroizing::new(format!("value = {}\n", value.as_str()));
                    let document = parse(&text)?;
                    if document.len() != 1 {
                        return Err(CodexNativeError::InvalidToml);
                    }
                    let item = document.get("value").ok_or(CodexNativeError::InvalidToml)?;
                    let boolean = matches!(
                        field.path.last().map(String::as_str),
                        Some("requires_openai_auth" | "supports_websockets")
                    );
                    if (boolean && !item.is_bool()) || (!boolean && !item.is_str()) {
                        return Err(CodexNativeError::UnsupportedLayout);
                    }
                    Ok(item.clone())
                })
                .transpose()?;
            fields.push(FieldRestore {
                path: field.path.clone(),
                before,
                after: field.after.clone(),
                passive_model: field.passive_model,
            });
        }
        for path in &record.created_tables {
            if !matches!(path.as_slice(), [parent] if parent == "model_providers")
                && !matches!(path.as_slice(), [parent, provider] if parent == "model_providers" && provider == "hiroute")
                && !matches!(path.as_slice(), [parent, provider, headers] if parent == "model_providers" && provider == "hiroute" && headers == "http_headers")
            {
                return Err(CodexNativeError::UnsupportedLayout);
            }
        }
        Ok(Self {
            fields,
            created_tables: record.created_tables,
            managed_aliases: record.managed_aliases,
        })
    }
}
