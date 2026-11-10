//! SMC decoding, key plans, averaging, retry and last-valid, over fakes. Nothing
//! here opens `IOKit`.

use super::*;
use crate::system::testing::FakeSmcKeys;

fn flt(value: f32) -> SmcRaw { SmcRaw::new(b"flt ", &value.to_le_bytes()) }

fn sp78(value: f64) -> SmcRaw {
    #[allow(clippy::cast_possible_truncation)]
    let raw = (value * 256.0) as i16;
    SmcRaw::new(b"sp78", &raw.to_be_bytes())
}

fn source(keys: &FakeSmcKeys, chip: ChipFamily) -> SmcCpuTemperature {
    SmcCpuTemperature::new(Box::new(keys.clone()), chip, SmcPolicy::default())
}

#[test]
fn every_supported_type_decodes() {
    assert_eq!(decode(&flt(48.5)), Some(48.5));
    assert_eq!(decode(&sp78(48.5)), Some(48.5));
    assert_eq!(decode(&SmcRaw::new(b"sp78", &[0xFF, 0x00])), Some(-1.0));
    assert_eq!(decode(&SmcRaw::new(b"fpe2", &[0x00, 0xC2])), Some(48.5));
    assert_eq!(decode(&SmcRaw::new(b"fp88", &[0x30, 0x80])), Some(48.5));
    assert_eq!(decode(&SmcRaw::new(b"ui8 ", &[48])), Some(48.0));
    assert_eq!(decode(&SmcRaw::new(b"ui16", &[0x01, 0x2C])), Some(300.0));
    assert_eq!(decode(&SmcRaw::new(b"ui32", &[0, 0, 0x01, 0x2C])), Some(300.0));
    assert_eq!(
        decode(&SmcRaw::new(b"ui64", &[0, 0, 0, 0, 0, 0, 0x01, 0x2C])),
        Some(300.0)
    );
    assert_eq!(decode(&SmcRaw::new(b"si8 ", &[0xFE])), Some(-2.0));
    assert_eq!(decode(&SmcRaw::new(b"si16", &[0xFF, 0xFE])), Some(-2.0));
}

#[test]
fn unknown_types_wrong_sizes_and_nonfinite_values_do_not_decode() {
    assert_eq!(decode(&SmcRaw::new(b"zzzz", &[1, 2])), None);
    assert_eq!(decode(&SmcRaw::new(b"flt ", &[1, 2])), None);
    assert_eq!(decode(&SmcRaw::new(b"flt ", &[])), None);
    assert_eq!(decode(&SmcRaw::new(b"sp78", &[1])), None);
    assert_eq!(decode(&SmcRaw::new(b"ui16", &[1])), None);
    assert_eq!(decode(&SmcRaw::new(b"ui8 ", &[])), None);
    assert_eq!(decode(&flt(f32::NAN)), None);
    assert_eq!(decode(&flt(f32::INFINITY)), None);
}

#[test]
fn keys_are_four_printable_ascii_bytes() {
    assert_eq!(SmcKey::parse("Tp01").map(SmcKey::code), Some(0x5470_3031));
    for bad in ["", "Tp0", "Tp012", "Tp 1", "Tp\u{e9}1"] {
        assert_eq!(SmcKey::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn the_chip_is_read_from_the_brand_string() {
    for (brand, chip) in [
        ("Apple M1", ChipFamily::M1),
        ("Apple M1 Max", ChipFamily::M1),
        ("Apple M2 Pro", ChipFamily::M2),
        ("Apple M3", ChipFamily::M3),
        ("Apple M4 Max", ChipFamily::M4),
        ("Apple M5", ChipFamily::M5),
        ("Apple M9 Ultra", ChipFamily::OtherApple),
        ("Apple Silicon", ChipFamily::OtherApple),
        ("Intel(R) Core(TM) i9-9980HK CPU @ 2.40GHz", ChipFamily::Intel),
        ("", ChipFamily::Intel),
    ] {
        assert_eq!(ChipFamily::from_brand(brand), chip, "{brand:?}");
    }
}

fn names(keys: &[SmcKey]) -> Vec<&str> { keys.iter().map(SmcKey::as_str).collect() }

#[test]
fn each_chip_reads_its_own_keys() {
    let m1 = ChipFamily::M1.plan();
    assert_eq!(names(&m1.primary).len(), 10);
    assert!(names(&m1.primary).contains(&"Tp0T"));
    assert_eq!(names(&m1.fallback), ["Tc0a", "Tc0b", "Tc0x", "Tc0z"]);

    let m2 = names(&ChipFamily::M2.plan().primary).join(" ");
    assert!(m2.contains("Tp1h") && m2.contains("Tp0n") && !m2.contains("Te05"));

    let m3 = names(&ChipFamily::M3.plan().primary).join(" ");
    assert!(m3.contains("Te05") && m3.contains("Tf04") && m3.contains("Tp0r"));

    let m4 = names(&ChipFamily::M4.plan().primary).join(" ");
    assert!(m4.contains("Te0H") && m4.contains("Tp0e") && !m4.contains("Tf04"));

    let m5 = ChipFamily::M5.plan();
    assert_eq!(names(&m5.primary).len(), 18);
    assert!(m5.fallback.is_empty());

    let intel = ChipFamily::Intel.plan();
    assert_eq!(names(&intel.primary)[0], "TC0C");
    assert_eq!(names(&intel.fallback), ["TC0D", "TC0E", "TC0F", "TC0P", "TCAD"]);

    let other = ChipFamily::OtherApple.plan();
    for key in ["Tp0T", "Tp1h", "Te05", "Tf04", "Tp00", "Tp0e"] {
        assert!(names(&other.primary).contains(&key), "{key}");
    }
    let mut unique = names(&other.primary);
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), other.primary.len(), "deduplicated");
}

#[test]
fn only_the_keys_of_the_chip_are_read() {
    let keys = FakeSmcKeys::default();
    keys.set("Tp01", flt(40.0)); // an M1 key
    keys.set("Tf04", flt(80.0)); // an M3-only key
    assert_eq!(source(&keys, ChipFamily::M1).cpu_temperature(), Ok(Some(40.0)));
    assert_eq!(source(&keys, ChipFamily::M3).cpu_temperature(), Ok(Some(60.0)));
    assert_eq!(source(&keys, ChipFamily::M5).cpu_temperature(), Ok(None));
}

#[test]
fn valid_keys_are_averaged_and_invalid_ones_are_skipped() {
    let keys = FakeSmcKeys::default();
    keys.set("Tp01", flt(40.0));
    keys.set("Tp05", sp78(60.0));
    keys.set("Tp0D", flt(0.0)); // not > 0
    keys.set("Tp0H", flt(150.0)); // not < 150
    keys.set("Tp0L", flt(-5.0));
    keys.set("Tp0P", flt(f32::NAN));
    keys.set("Tp0X", SmcRaw::new(b"zzzz", &[1, 2]));
    // An absent key is skipped too.
    assert_eq!(source(&keys, ChipFamily::M1).cpu_temperature(), Ok(Some(50.0)));
}

#[test]
fn the_fallback_keys_are_used_only_when_no_primary_key_is_valid() {
    let keys = FakeSmcKeys::default();
    keys.set("Tc0a", flt(30.0));
    keys.set("Tc0b", flt(50.0));
    assert_eq!(source(&keys, ChipFamily::M1).cpu_temperature(), Ok(Some(40.0)));
    keys.set("Tp01", flt(70.0));
    assert_eq!(source(&keys, ChipFamily::M1).cpu_temperature(), Ok(Some(70.0)));
}

#[test]
fn intel_averages_core_keys_then_falls_back_to_the_package_keys() {
    let keys = FakeSmcKeys::default();
    keys.set("TC0D", sp78(70.0));
    assert_eq!(
        source(&keys, ChipFamily::Intel).cpu_temperature(),
        Ok(Some(70.0))
    );
    keys.set("TC0C", sp78(40.0));
    keys.set("TC1C", sp78(50.0));
    assert_eq!(
        source(&keys, ChipFamily::Intel).cpu_temperature(),
        Ok(Some(45.0))
    );
}

#[test]
fn no_keys_means_unknown_and_never_a_number() {
    let keys = FakeSmcKeys::default();
    for chip in [
        ChipFamily::Intel,
        ChipFamily::M1,
        ChipFamily::M2,
        ChipFamily::OtherApple,
    ] {
        assert_eq!(source(&keys, chip).cpu_temperature(), Ok(None), "{chip:?}");
    }
}

#[test]
fn an_open_failure_is_an_error_and_is_retried_on_the_next_sample() {
    let keys = FakeSmcKeys::default();
    keys.set("Tp01", flt(40.0));
    keys.fail_open(Some(SystemReadError::new("no smc")));
    let smc = source(&keys, ChipFamily::M1);
    assert!(smc.cpu_temperature().is_err());
    assert!(smc.cpu_temperature().is_err());
    assert_eq!(keys.opens(), 2);
    assert_eq!(keys.reads(), 0);
    keys.fail_open(None);
    assert_eq!(smc.cpu_temperature(), Ok(Some(40.0)));
    // The connection is kept while it works.
    assert_eq!(smc.cpu_temperature(), Ok(Some(40.0)));
    assert_eq!(keys.opens(), 3);
}

#[test]
fn a_read_failure_drops_the_connection_and_reopens() {
    let keys = FakeSmcKeys::default();
    keys.set("Tp01", flt(40.0));
    let smc = source(&keys, ChipFamily::M1);
    assert_eq!(smc.cpu_temperature(), Ok(Some(40.0)));
    keys.fail_reads(Some(SystemReadError::new("io")));
    // The last valid value stands in for a bounded number of samples.
    assert_eq!(smc.cpu_temperature(), Ok(Some(40.0)));
    keys.fail_reads(None);
    assert_eq!(smc.cpu_temperature(), Ok(Some(40.0)));
    assert_eq!(keys.opens(), 2);
}

#[test]
fn an_invalid_group_is_retried_a_bounded_number_of_times_within_one_sample() {
    let keys = FakeSmcKeys::default();
    // The first two group reads see only invalid values, then a good one.
    keys.push("Tp01", flt(0.0));
    keys.push("Tp01", flt(0.0));
    keys.push("Tp01", flt(42.0));
    let smc = source(&keys, ChipFamily::M1);
    assert_eq!(smc.cpu_temperature(), Ok(Some(42.0)));

    // Never more than 1 + retries group reads: the M1 plan has 10 primary and
    // 4 fallback keys.
    let always_bad = FakeSmcKeys::default();
    always_bad.set("Tp01", flt(0.0));
    let smc = source(&always_bad, ChipFamily::M1);
    assert_eq!(smc.cpu_temperature(), Ok(None));
    assert_eq!(always_bad.reads(), (10 + 4) * (1 + DEFAULT_RETRIES as usize));
}

#[test]
fn the_last_valid_value_is_repeated_for_exactly_the_stale_bound_then_unknown() {
    let keys = FakeSmcKeys::default();
    keys.set("Tp01", flt(55.0));
    let smc = source(&keys, ChipFamily::M1);
    assert_eq!(smc.cpu_temperature(), Ok(Some(55.0)));
    keys.clear();
    for _ in 0..DEFAULT_STALE_SAMPLES {
        assert_eq!(smc.cpu_temperature(), Ok(Some(55.0)));
    }
    assert_eq!(smc.cpu_temperature(), Ok(None));
    assert_eq!(smc.cpu_temperature(), Ok(None), "and it stays unknown");
    keys.set("Tp01", flt(61.0));
    assert_eq!(
        smc.cpu_temperature(),
        Ok(Some(61.0)),
        "a fresh valid reading recovers"
    );
}

#[test]
fn a_fresh_valid_reading_resets_the_stale_count() {
    let keys = FakeSmcKeys::default();
    keys.set("Tp01", flt(55.0));
    let smc = source(&keys, ChipFamily::M1);
    assert_eq!(smc.cpu_temperature(), Ok(Some(55.0)));
    keys.clear();
    assert_eq!(smc.cpu_temperature(), Ok(Some(55.0)));
    assert_eq!(smc.cpu_temperature(), Ok(Some(55.0)));
    keys.set("Tp01", flt(56.0));
    assert_eq!(smc.cpu_temperature(), Ok(Some(56.0)));
    keys.clear();
    for _ in 0..DEFAULT_STALE_SAMPLES {
        assert_eq!(smc.cpu_temperature(), Ok(Some(56.0)));
    }
    assert_eq!(smc.cpu_temperature(), Ok(None));
}

#[test]
fn an_error_after_the_stale_bound_is_reported_not_hidden() {
    let keys = FakeSmcKeys::default();
    keys.set("Tp01", flt(55.0));
    let smc = source(&keys, ChipFamily::M1);
    assert_eq!(smc.cpu_temperature(), Ok(Some(55.0)));
    keys.fail_open(Some(SystemReadError::new("gone")));
    keys.fail_reads(Some(SystemReadError::new("gone")));
    for _ in 0..DEFAULT_STALE_SAMPLES {
        assert_eq!(smc.cpu_temperature(), Ok(Some(55.0)));
    }
    assert!(smc.cpu_temperature().is_err());
}
