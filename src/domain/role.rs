use serde::{Deserialize, Serialize};

/// Engineering responsibility assigned to a stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Researcher,
    Architect,
    Implementer,
    Reviewer,
    EngineeringLead,
}

#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn role_serialization_contains_no_provider_or_model_vocabulary() {
        assert_eq!(
            serde_json::to_string(&Role::EngineeringLead).unwrap(),
            "\"engineering_lead\""
        );
    }
}
