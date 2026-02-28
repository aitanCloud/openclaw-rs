use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Who triggered a state-changing event. Included in every state-changing event payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Actor {
    Human { actor_id: String },
    Planner,
    Worker { run_id: Uuid },
    System,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(actor: &Actor) -> Actor {
        let json = serde_json::to_string(actor).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn serde_human() {
        let actor = Actor::Human {
            actor_id: "user-42".to_string(),
        };
        let restored = round_trip(&actor);
        assert_eq!(actor, restored);

        let json = serde_json::to_string(&actor).unwrap();
        assert!(json.contains(r#""kind":"Human"#));
        assert!(json.contains(r#""actor_id":"user-42"#));
    }

    #[test]
    fn serde_planner() {
        let actor = Actor::Planner;
        let restored = round_trip(&actor);
        assert_eq!(actor, restored);

        let json = serde_json::to_string(&actor).unwrap();
        assert!(json.contains(r#""kind":"Planner"#));
    }

    #[test]
    fn serde_worker() {
        let run_id = Uuid::new_v4();
        let actor = Actor::Worker { run_id };
        let restored = round_trip(&actor);
        assert_eq!(actor, restored);

        let json = serde_json::to_string(&actor).unwrap();
        assert!(json.contains(r#""kind":"Worker"#));
        assert!(json.contains(&run_id.to_string()));
    }

    #[test]
    fn serde_system() {
        let actor = Actor::System;
        let restored = round_trip(&actor);
        assert_eq!(actor, restored);

        let json = serde_json::to_string(&actor).unwrap();
        assert!(json.contains(r#""kind":"System"#));
    }

    #[test]
    fn deserialize_from_known_json() {
        let json = r#"{"kind":"Human","actor_id":"alice"}"#;
        let actor: Actor = serde_json::from_str(json).unwrap();
        assert_eq!(
            actor,
            Actor::Human {
                actor_id: "alice".to_string()
            }
        );
    }

    #[test]
    fn clone_and_eq() {
        let a = Actor::Planner;
        let b = a.clone();
        assert_eq!(a, b);
    }
}
