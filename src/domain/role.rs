use serde::{Deserialize, Serialize};

/// Engineering responsibility assigned to a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Researcher,
    Architect,
    Implementer,
    CodeQualityReviewer,
    SpecReviewer,
    /// Legacy/general review responsibility retained for persisted runs.
    Reviewer,
    EngineeringLead,
}

#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn role_serialization_contains_no_provider_or_model_vocabulary() {
        assert_eq!(
            serde_json::to_string(&Role::CodeQualityReviewer).unwrap(),
            "\"code_quality_reviewer\""
        );
        assert_eq!(
            serde_json::to_string(&Role::SpecReviewer).unwrap(),
            "\"spec_reviewer\""
        );
        assert_eq!(
            serde_json::to_string(&Role::EngineeringLead).unwrap(),
            "\"engineering_lead\""
        );
    }

    #[test]
    fn legacy_reviewer_deserializes_without_conversion() {
        assert_eq!(
            serde_json::from_str::<Role>("\"reviewer\"").unwrap(),
            Role::Reviewer
        );
    }
}
