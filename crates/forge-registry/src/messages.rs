//! User-facing strings (pt-BR), kept in one place until the i18n infrastructure exists
//! (docs/README conventions). Code, logs and errors stay in English.

use crate::index::Verification;

/// Badge of a registry whose index signature was checked.
pub const VERIFIED: &str = "verificado";
/// Badge of a registry that loaded without a valid signature (`trusted_unsigned`).
pub const UNVERIFIED: &str = "não verificado";
/// Badge of a registry that is not the official one.
pub const THIRD_PARTY: &str = "terceiro";
/// Badge of the official registry.
pub const OFFICIAL: &str = "oficial";

/// Label shown next to a registry for its [`Verification`].
pub fn verification_label(verification: &Verification) -> &'static str {
    match verification {
        Verification::Verified { .. } => VERIFIED,
        Verification::Unverified => UNVERIFIED,
    }
}

/// Label for the origin badge of a registry.
pub fn origin_label(official: bool) -> &'static str {
    if official { OFFICIAL } else { THIRD_PARTY }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_state() {
        assert_eq!(
            verification_label(&Verification::Verified {
                key_id: "A".to_owned()
            }),
            "verificado"
        );
        assert_eq!(
            verification_label(&Verification::Unverified),
            "não verificado"
        );
        assert_eq!(origin_label(true), "oficial");
        assert_eq!(origin_label(false), "terceiro");
    }
}
