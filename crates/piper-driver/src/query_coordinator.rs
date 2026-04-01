use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QueryKind {
    JointLimit = 1,
    JointAccel = 2,
    EndLimit = 3,
    CollisionProtection = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveQuery {
    pub token: u64,
    pub kind: QueryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    #[error("query coordinator busy")]
    Busy,
}

#[derive(Debug)]
pub struct QueryCoordinator {
    active: Mutex<Option<ActiveQuery>>,
    next_token: AtomicU64,
}

#[derive(Debug)]
pub struct QueryGuard<'a> {
    coordinator: &'a QueryCoordinator,
    token: u64,
}

impl QueryCoordinator {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
            next_token: AtomicU64::new(0),
        }
    }

    pub fn try_begin(&self, kind: QueryKind) -> Result<QueryGuard<'_>, QueryError> {
        let mut active = self.active.lock().unwrap_or_else(|poison| poison.into_inner());

        if active.is_some() {
            return Err(QueryError::Busy);
        }

        let token = self.next_token.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        *active = Some(ActiveQuery { token, kind });

        Ok(QueryGuard {
            coordinator: self,
            token,
        })
    }

    pub fn active_query(&self) -> Option<ActiveQuery> {
        self.active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .copied()
    }

    fn clear_if_matches(&self, token: u64) {
        let mut active = self.active.lock().unwrap_or_else(|poison| poison.into_inner());

        if active.as_ref().map(|query| query.token) == Some(token) {
            *active = None;
        }
    }
}

impl Default for QueryCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Drop for QueryGuard<'a> {
    fn drop(&mut self) {
        self.coordinator.clear_if_matches(self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_coordinator_is_fail_fast_when_busy() {
        let coordinator = QueryCoordinator::new();
        let _guard = coordinator.try_begin(QueryKind::JointLimit).unwrap();
        let err = coordinator.try_begin(QueryKind::CollisionProtection).unwrap_err();

        assert_eq!(err, QueryError::Busy);
    }

    #[test]
    fn active_query_is_observable_while_guard_is_alive() {
        let coordinator = QueryCoordinator::new();
        let guard = coordinator.try_begin(QueryKind::JointLimit).unwrap();

        assert_eq!(
            coordinator.active_query(),
            Some(ActiveQuery {
                token: 1,
                kind: QueryKind::JointLimit,
            })
        );

        drop(guard);
        assert_eq!(coordinator.active_query(), None);
    }

    #[test]
    fn guard_drop_only_clears_matching_token() {
        let coordinator = QueryCoordinator::new();
        let guard = coordinator.try_begin(QueryKind::JointLimit).unwrap();
        let active = coordinator.active_query().unwrap();

        {
            let mut slot = coordinator.active.lock().unwrap();
            *slot = Some(ActiveQuery {
                token: active.token + 1,
                kind: QueryKind::CollisionProtection,
            });
        }

        drop(guard);

        assert_eq!(
            coordinator.active_query(),
            Some(ActiveQuery {
                token: active.token + 1,
                kind: QueryKind::CollisionProtection,
            })
        );
    }
}
