//! Typed facades over the same Local Control transport used by the CLI and Desktop.
use crate::{Client, ClientFailure};
use hiroute_application_api::*;
impl Client {
    pub async fn model_reference(
        &self,
        request_id: &str,
        model_configuration_id: String,
    ) -> Result<MachineEnvelopeV2<ModelCatalogResultV1>, ClientFailure> {
        self.query(
            "ShowModel",
            request_id,
            &ModelCatalogQueryV1::Reference {
                model_configuration_id,
            },
        )
        .await
    }
    pub async fn model_ratings(
        &self,
        request_id: &str,
        query: ResolveModelRatingsV1,
    ) -> Result<MachineEnvelopeV2<ModelCatalogResultV1>, ClientFailure> {
        self.query(
            "ShowModel",
            request_id,
            &ModelCatalogQueryV1::Ratings { query },
        )
        .await
    }
    pub async fn effective_prices(
        &self,
        request_id: &str,
        query: GetEffectivePricesV2,
    ) -> Result<MachineEnvelopeV2<EffectivePricesResultV2>, ClientFailure> {
        self.query("GetEffectivePrices", request_id, &query).await
    }
    pub async fn preview_source_price(
        &self,
        request_id: &str,
        change: PreviewPriceOverrideChangeV2,
    ) -> Result<MachineEnvelopeV2<PriceOverridePreviewV2>, ClientFailure> {
        self.query("PreviewPriceOverrideChange", request_id, &change)
            .await
    }
}
