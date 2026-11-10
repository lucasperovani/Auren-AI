//! Internal service layer (spec section 9).
//!
//! The REST API, conversational agent, scheduler, and skill installer share
//! these services so that security and business rules are enforced in one
//! place. All state changes are persisted before being reported as successful.

pub mod approvals;
pub mod audit;
pub mod agent_runtime;
pub mod browser;
pub mod builtins;
pub mod capabilities;
pub mod credentials;
pub mod google_oauth;
pub mod github_oauth;
pub mod integrations;
pub mod llm;
pub mod mcp;
pub mod notifications;
pub mod projects;
pub mod rate_limits;
pub mod scheduler;
pub mod skill_import;
pub mod skill_installer;
pub mod task;
pub mod task_collab;
pub mod task_tools;
pub mod tool_registry;
pub mod tool_sync;
pub mod web_research;
pub mod worker;

#[cfg(test)]
mod browser_tests;
#[cfg(test)]
mod web_research_tests;
#[cfg(test)]
mod skill_installer_tests;