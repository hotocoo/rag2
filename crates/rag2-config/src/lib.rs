//! RAG2 configuration engine.
//!
//! Operators describe the whole platform in one declarative profile (YAML or JSON):
//! [`schema::Profile`]. Profiles are validated ([`validate`]), stored as immutable versions
//! ([`store`]), previewed as diffs against what is running, activated atomically, rolled back, and
//! hot-reloaded from a file ([`watch`]). Every default lives in [`defaults`].

pub mod defaults;
pub mod manager;
pub mod patch;
pub mod schema;
pub mod store;
pub mod validate;
pub mod watch;

pub use manager::{Active, ConfigManager, Preview, parse_profile_value, resolve};
pub use schema::{API_VERSION, Profile, json_schema};
pub use store::{ConfigStore, FsConfigStore};
pub use validate::{Issue, Severity, validate};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_defaults_from_an_empty_object() {
        // Profile::minimal calls each section's Default, which deserializes `{}`; a section with
        // a required field would panic here instead of in production.
        let p = Profile::minimal("x");
        assert!(validate(&p).iter().all(|i| i.severity != Severity::Error));
    }

    #[test]
    fn the_json_schema_describes_profiles() {
        let s = json_schema();
        let text = s.to_string();
        assert!(text.contains("api_version") && text.contains("pipelines"));
    }

    #[test]
    fn a_profile_round_trips_through_yaml() {
        let p = Profile::minimal("x");
        let y = serde_norway::to_string(&p).expect("to yaml");
        let back = resolve(&parse_profile_value(&y).expect("parse"), None).expect("resolve");
        assert_eq!(p, back);
    }
}
