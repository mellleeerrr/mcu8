//! Operator decision log for patent-encumbered codec bridges.
//!
//! The FFmpeg probe only proves that a codec *can* run on this host; it must
//! never enable a covered codec on its own. The deployer records the decision
//! explicitly:
//!
//! * `VKS_CODEC_LICENSE_ACCEPTED=aac-latm[,g729…]` — comma-separated list of
//!   codecs the operator takes responsibility for;
//! * `VKS_AAC_LATM_LEGAL_ACCEPTED=1` — legacy single-codec alias, kept so
//!   existing deployments do not silently lose AAC after the upgrade.
//!
//! Unknown tokens are ignored (and logged by the caller); an empty list means
//! "nothing accepted". Parsing is deliberately pure so it is unit-testable
//! without touching the process environment.

/// Codecs that currently have a working bridge and a licensing gate.
pub const AAC_LATM: &str = "aac-latm";

/// Parses the accepted-codec list. Only codecs this server knows how to gate
/// are returned; the result is deduplicated and order-preserving.
#[must_use]
pub fn accepted_codecs(list: Option<&str>, legacy_aac_flag: Option<&str>) -> Vec<&'static str> {
    let mut accepted = Vec::new();
    let known: Vec<&'static str> =
        vks_common::codec_spec::CODECS.iter().filter_map(|spec| spec.license.token()).collect();
    for token in list.unwrap_or_default().split(',') {
        let token = token.trim();
        // Only tokens that belong to a license-gated spec are accepted; the
        // list of known tokens comes from the SSOT, not a literal here.
        if let Some(known) = known.iter().copied().find(|known| token.eq_ignore_ascii_case(known)) {
            if !accepted.contains(&known) {
                accepted.push(known);
            }
        }
    }
    // Legacy flag maps to the one legacy-gated codec; keep it in sync with the
    // spec token rather than duplicating the literal.
    let legacy_token = vks_common::codec_spec::spec(vks_common::codec_spec::CodecId::AacLatm).license.token();
    let legacy_on = legacy_aac_flag.is_some_and(|value| value.trim() == "1");
    if legacy_on {
        if let Some(token) = legacy_token {
            if !accepted.contains(&token) {
                accepted.push(token);
            }
        }
    }
    accepted
}

/// Reads both variables from the process environment.
#[must_use]
pub fn accepted_codecs_from_env() -> Vec<&'static str> {
    let list = std::env::var("VKS_CODEC_LICENSE_ACCEPTED").ok();
    let legacy = std::env::var("VKS_AAC_LATM_LEGAL_ACCEPTED").ok();
    accepted_codecs(list.as_deref(), legacy.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_missing_list_accepts_nothing() {
        assert!(accepted_codecs(None, None).is_empty());
        assert!(accepted_codecs(Some(""), None).is_empty());
        assert!(accepted_codecs(Some(" , "), None).is_empty());
    }

    #[test]
    fn aac_latm_is_accepted_by_name_case_and_whitespace_insensitively() {
        for value in ["aac-latm", " AAC-LATM ", "aac-latm,aac-latm", "g729, aac-latm"] {
            assert_eq!(accepted_codecs(Some(value), None), vec![AAC_LATM], "list {value:?}");
        }
    }

    #[test]
    fn unknown_tokens_and_garbage_fail_closed() {
        for value in ["aac", "mp4a-latm", "not-a-codec", "aac-latm=1", "g729"] {
            assert!(accepted_codecs(Some(value), None).is_empty(), "garbage {value:?} must not opt in");
        }
    }

    #[test]
    fn legacy_alias_still_enables_aac_latm_but_only_for_exact_one() {
        assert_eq!(accepted_codecs(None, Some("1")), vec![AAC_LATM]);
        assert_eq!(accepted_codecs(None, Some(" 1 ")), vec![AAC_LATM]);
        for value in ["0", "true", "yes", "", "2"] {
            assert!(accepted_codecs(None, Some(value)).is_empty(), "legacy {value:?} must not opt in");
        }
        // No duplicates when both forms are present.
        assert_eq!(accepted_codecs(Some("aac-latm"), Some("1")), vec![AAC_LATM]);
    }
    #[test]
    fn license_tokens_come_from_specs() {
        let tokens: Vec<&str> = vks_common::codec_spec::CODECS.iter().filter_map(|spec| spec.license.token()).collect();
        assert_eq!(tokens, vec![AAC_LATM]);
        assert_eq!(accepted_codecs(Some("aac-latm"), None), tokens);
        assert_eq!(accepted_codecs(None, Some("1")), tokens);
        // A token that is not a license token stays rejected.
        assert!(accepted_codecs(Some("g729"), None).is_empty());
    }
}
