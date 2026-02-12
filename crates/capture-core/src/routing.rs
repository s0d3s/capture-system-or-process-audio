use crate::traits::CapabilityError;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Ownership record for a single stream, node, or device route modified when `allow_reroute = true`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedRoute {
    pub subject_id: u32,
    pub original_destination: Arc<str>,
    pub temporary_destination: Arc<str>,
    pub was_muted: bool,
}

/// Cross-platform route-restoration guard enforcing non-disruptive capture by default
/// (`allow_reroute = false`) and restoring **only** library-owned route/mute/sink changes on
/// partial-open failure or teardown without overwriting concurrent user route moves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteRollbackGuard {
    allow_reroute: bool,
    owned_objects: Vec<u32>,
    moved_routes: BTreeMap<u32, OwnedRoute>,
}

impl RouteRollbackGuard {
    pub fn new(allow_reroute: bool) -> Self {
        Self {
            allow_reroute,
            owned_objects: Vec::new(),
            moved_routes: BTreeMap::new(),
        }
    }

    #[inline]
    pub fn allow_reroute(&self) -> bool {
        self.allow_reroute
    }

    #[inline]
    pub fn owned_objects(&self) -> &[u32] {
        &self.owned_objects
    }

    #[inline]
    pub fn moved_routes(&self) -> &BTreeMap<u32, OwnedRoute> {
        &self.moved_routes
    }

    /// Rejects disruptive route/mute behavior (`requires_reroute == true`) when `allow_reroute == false`.
    pub fn require_reroute_permitted(&self, requires_reroute: bool) -> Result<(), CapabilityError> {
        if requires_reroute && !self.allow_reroute {
            return Err(CapabilityError::RerouteNotPermitted);
        }
        Ok(())
    }

    /// Track an owned temporary native object (virtual sink, module, tap, or aggregate device)
    /// so `plan_rollback` destroys/unloads owned objects in deterministic LIFO order.
    pub fn record_owned_object(&mut self, object_id: u32) {
        if !self.owned_objects.contains(&object_id) {
            self.owned_objects.push(object_id);
        }
    }

    /// Record a stream or object route/mute modification. Rejects with `RerouteNotPermitted`
    /// when `allow_reroute == false`.
    pub fn record_route_change(
        &mut self,
        subject_id: u32,
        original_destination: impl Into<Arc<str>>,
        temporary_destination: impl Into<Arc<str>>,
        was_muted: bool,
    ) -> Result<(), CapabilityError> {
        if !self.allow_reroute {
            return Err(CapabilityError::RerouteNotPermitted);
        }
        self.moved_routes.insert(
            subject_id,
            OwnedRoute {
                subject_id,
                original_destination: original_destination.into(),
                temporary_destination: temporary_destination.into(),
                was_muted,
            },
        );
        Ok(())
    }

    /// Compute deterministic rollback actions `(restore_moves, destroy_object_ids_lifo)` given
    /// the current live `subject_id -> destination` map. Only subjects still pointing to
    /// `temporary_destination` are moved back to `original_destination`; concurrent user route
    /// changes are preserved.
    pub fn plan_rollback(
        &mut self,
        live_destinations: &BTreeMap<u32, Arc<str>>,
    ) -> (Vec<(u32, Arc<str>)>, Vec<u32>) {
        let mut restores = Vec::new();
        for (subject_id, record) in std::mem::take(&mut self.moved_routes) {
            if let Some(current_dest) = live_destinations.get(&subject_id)
                && current_dest == &record.temporary_destination
            {
                restores.push((subject_id, record.original_destination));
            }
        }
        let mut destroy_ids = std::mem::take(&mut self.owned_objects);
        destroy_ids.reverse();
        (restores, destroy_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_rollback_guard_enforces_opt_in_and_preserves_concurrent_user_moves() {
        let mut disallowed = RouteRollbackGuard::new(false);
        assert_eq!(disallowed.require_reroute_permitted(false), Ok(()));
        assert_eq!(
            disallowed.require_reroute_permitted(true),
            Err(CapabilityError::RerouteNotPermitted)
        );
        assert_eq!(
            disallowed.record_route_change(10, "speakers", "cspa-temp-sink", false),
            Err(CapabilityError::RerouteNotPermitted)
        );
        assert!(disallowed.moved_routes().is_empty());

        let mut allowed = RouteRollbackGuard::new(true);
        assert_eq!(allowed.require_reroute_permitted(true), Ok(()));
        allowed.record_owned_object(101);
        allowed.record_owned_object(102);
        allowed.record_owned_object(101); // idempotent duplicate ignored
        allowed
            .record_route_change(10, "speakers", "cspa-temp-sink", false)
            .unwrap();
        allowed
            .record_route_change(11, "speakers", "cspa-temp-sink", true)
            .unwrap();

        let mut live = BTreeMap::new();
        live.insert(10, Arc::<str>::from("cspa-temp-sink"));
        live.insert(11, Arc::<str>::from("user-headphones"));

        let (restores, destroys_lifo) = allowed.plan_rollback(&live);
        assert_eq!(restores, vec![(10, Arc::<str>::from("speakers"))]);
        assert_eq!(destroys_lifo, vec![102, 101]);
        assert!(allowed.moved_routes().is_empty());
        assert!(allowed.owned_objects().is_empty());
    }
}
