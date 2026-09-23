//! `oneinch` vendor adapter. Owner: `market-trading` (Phase 1). See plan/TASKS.md.
//!
//! Implement the ports this vendor supports and push one `Registration` in [`register`].
//! Vendor DTOs stay private to this module (anti-corruption layer).

use ems_config::Loaded;
use ems_ports::Registration;

/// Push this vendor's registration if it is active (`loaded.vendor_status("oneinch")`).
pub fn register(_loaded: &Loaded, _out: &mut Vec<Registration>) {}
