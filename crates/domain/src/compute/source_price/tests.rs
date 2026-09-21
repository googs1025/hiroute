use super::*;
#[test]
fn source_price_decimal_is_exact_and_rejects_truncation_and_overflow() {
    assert_eq!(
        TokenRateV1::parse_decimal("1.20").unwrap(),
        TokenRateV1::known(1_200_000)
    );
    assert_eq!(
        TokenRateV1::parse_decimal("0").unwrap(),
        TokenRateV1::known(0)
    );
    assert_eq!(
        TokenRateV1::parse_decimal("18446744073709.551615").unwrap(),
        TokenRateV1::known(u64::MAX)
    );
    for value in [
        "",
        "-1",
        "+1",
        " 1",
        "NaN",
        "1e3",
        "1.",
        ".1",
        "1.0000001",
        "18446744073709.551616",
        "1.2.3",
    ] {
        assert!(TokenRateV1::parse_decimal(value).is_err(), "{value}");
    }
}
#[test]
fn source_price_basic_pair_does_not_imply_cache_price() {
    let rates = TokenRatesV1::from_legacy(1_200_000, 4_800_000);
    rates.validate_manual().unwrap();
    assert_eq!(
        rates.cache_read,
        TokenRateV1::unknown(PriceUnknownReasonV1::CacheRateNotCollected)
    );
    assert_eq!(
        rates.unknown_reasons(),
        vec![PriceUnknownReasonV1::CacheRateNotCollected]
    );
    assert!(
        TokenRatesV1::unknown(PriceUnknownReasonV1::PriceNotCollected)
            .validate_manual()
            .is_err()
    );
}
