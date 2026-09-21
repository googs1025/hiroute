use hiroute_domain::{ModelSwitchFilter, ObservationQueryError, SessionListQueryV1};

pub(super) fn validate_list_query(query: &SessionListQueryV1) -> Result<(), ObservationQueryError> {
    if query
        .from_ms
        .zip(query.to_ms)
        .is_some_and(|(from, to)| from > to)
        || query
            .query
            .as_ref()
            .is_some_and(|value| value.trim().len() > 256)
    {
        Err(ObservationQueryError::InvalidQuery)
    } else {
        Ok(())
    }
}

pub(super) const fn matches_switch(filter: ModelSwitchFilter, value: Option<bool>) -> bool {
    match filter {
        ModelSwitchFilter::Any => true,
        ModelSwitchFilter::Only => matches!(value, Some(true)),
        ModelSwitchFilter::Exclude => matches!(value, Some(false)),
    }
}
