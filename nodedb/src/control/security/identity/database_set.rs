// SPDX-License-Identifier: BUSL-1.1

#![deny(clippy::wildcard_enum_match_arm)]

use smallvec::SmallVec;

use nodedb_types::id::DatabaseId;

/// The set of databases this identity is permitted to access.
///
/// `All` means no restriction (e.g. superuser). `Some` enumerates the exact
/// databases. Session bind rejects any `current_database` not in the `Some`
/// set with `ACCESS_DENIED`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseSet {
    /// No restriction — every database is accessible.
    All,
    /// Exactly these databases are accessible (inline-allocated, spills to heap
    /// only when a user has more than 4 explicit grants).
    Some(SmallVec<[DatabaseId; 4]>),
}

impl DatabaseSet {
    /// Returns `true` if the given database is accessible.
    pub fn contains(&self, db: DatabaseId) -> bool {
        match self {
            DatabaseSet::All => true,
            DatabaseSet::Some(ids) => ids.contains(&db),
        }
    }

    /// Intersect two database sets. The result contains only databases
    /// accessible in both sets. `All ∩ x == x`. `Some(a) ∩ Some(b) == Some(a ∩ b)`.
    ///
    /// Match is exhaustive — no `_ =>` arms.
    pub fn intersect(&self, other: &DatabaseSet) -> DatabaseSet {
        match (self, other) {
            (DatabaseSet::All, DatabaseSet::All) => DatabaseSet::All,
            (DatabaseSet::All, DatabaseSet::Some(ids)) => DatabaseSet::Some(ids.clone()),
            (DatabaseSet::Some(ids), DatabaseSet::All) => DatabaseSet::Some(ids.clone()),
            (DatabaseSet::Some(a), DatabaseSet::Some(b)) => {
                let intersection: SmallVec<[DatabaseId; 4]> =
                    a.iter().filter(|id| b.contains(id)).copied().collect();
                DatabaseSet::Some(intersection)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_set_contains() {
        let db1 = DatabaseId::new(1);
        let db2 = DatabaseId::new(2);
        let set = DatabaseSet::Some(smallvec::smallvec![db1]);
        assert!(set.contains(db1));
        assert!(!set.contains(db2));
        assert!(DatabaseSet::All.contains(db2));
    }

    #[test]
    fn database_set_intersect_all_all() {
        assert_eq!(
            DatabaseSet::All.intersect(&DatabaseSet::All),
            DatabaseSet::All
        );
    }

    #[test]
    fn database_set_intersect_all_some() {
        let db1 = DatabaseId::new(1);
        let some = DatabaseSet::Some(smallvec::smallvec![db1]);
        assert_eq!(DatabaseSet::All.intersect(&some), some);
    }

    #[test]
    fn database_set_intersect_some_all() {
        let db1 = DatabaseId::new(1);
        let some = DatabaseSet::Some(smallvec::smallvec![db1]);
        assert_eq!(some.intersect(&DatabaseSet::All), some);
    }

    #[test]
    fn database_set_intersect_some_some_overlap() {
        let db1 = DatabaseId::new(1);
        let db2 = DatabaseId::new(2);
        let a = DatabaseSet::Some(smallvec::smallvec![db1, db2]);
        let b = DatabaseSet::Some(smallvec::smallvec![db1]);
        let result = a.intersect(&b);
        assert_eq!(result, DatabaseSet::Some(smallvec::smallvec![db1]));
    }

    #[test]
    fn database_set_intersect_some_some_disjoint() {
        let db1 = DatabaseId::new(1);
        let db2 = DatabaseId::new(2);
        let a = DatabaseSet::Some(smallvec::smallvec![db1]);
        let b = DatabaseSet::Some(smallvec::smallvec![db2]);
        let result = a.intersect(&b);
        assert_eq!(result, DatabaseSet::Some(smallvec::smallvec![]));
    }
}
