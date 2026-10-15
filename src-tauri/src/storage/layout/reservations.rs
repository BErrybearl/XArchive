use super::super::*;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
};

#[derive(Clone, Eq)]
struct PathName {
    #[cfg(windows)]
    value: Vec<u16>,
    #[cfg(not(windows))]
    value: String,
}
impl PathName {
    fn new(raw: &str) -> Self {
        Self {
            #[cfg(windows)]
            value: raw.encode_utf16().collect(),
            #[cfg(not(windows))]
            value: raw.to_owned(),
        }
    }
}
impl Ord for PathName {
    fn cmp(&self, other: &Self) -> Ordering {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Globalization::{
                CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal,
            };
            if let (Ok(left_len), Ok(right_len)) = (
                i32::try_from(self.value.len()),
                i32::try_from(other.value.len()),
            ) {
                // Prepared UTF-16 keys avoid allocations on each lookup. Ordinal
                // casing also handles non-ASCII names in imported libraries.
                match unsafe {
                    CompareStringOrdinal(
                        self.value.as_ptr(),
                        left_len,
                        other.value.as_ptr(),
                        right_len,
                        1,
                    )
                } {
                    CSTR_LESS_THAN => return Ordering::Less,
                    CSTR_EQUAL => return Ordering::Equal,
                    CSTR_GREATER_THAN => return Ordering::Greater,
                    _ => {}
                }
            }
        }
        self.value.cmp(&other.value)
    }
}
impl PartialOrd for PathName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for PathName {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
pub(super) fn same_path_name(left: &str, right: &str) -> bool {
    PathName::new(left) == PathName::new(right)
}

type Owners = BTreeMap<PathName, HashSet<i64>>;

/// A task's single writer maintains these reservations alongside committed SQL.
/// Missing indexed files and pending moves both keep their names occupied.
#[derive(Default)]
pub(crate) struct PathReservations {
    indexed: Owners,
    pending: Owners,
    entries: HashMap<i64, (PathName, Option<PathName>)>,
}
impl PathReservations {
    pub(crate) fn load(db: &Connection) -> Result<Self> {
        let mut result = Self::default();
        let mut q = db.prepare("SELECT e.entity_id,e.relative_path,l.new_path FROM media_entities e LEFT JOIN media_rename_log l ON l.entity_id=e.entity_id AND l.state<>'done'")?;
        for row in q.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })? {
            let (id, path, pending) = row?;
            result.insert(id, &path, pending.as_deref());
        }
        Ok(result)
    }
    fn insert(&mut self, id: i64, raw: &str, pending: Option<&str>) {
        let path = PathName::new(raw);
        let pending = pending.map(PathName::new);
        self.indexed.entry(path.clone()).or_default().insert(id);
        if let Some(path) = &pending {
            self.pending.entry(path.clone()).or_default().insert(id);
        }
        self.entries.insert(id, (path, pending));
    }
    pub(crate) fn refresh(&mut self, db: &Connection, id: i64) -> Result<()> {
        let row: Option<(String, Option<String>)> = db.query_row("SELECT e.relative_path,l.new_path FROM media_entities e LEFT JOIN media_rename_log l ON l.entity_id=e.entity_id AND l.state<>'done' WHERE e.entity_id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((path, pending)) = self.entries.remove(&id) {
            remove_owner(&mut self.indexed, &path, id);
            if let Some(path) = pending {
                remove_owner(&mut self.pending, &path, id);
            }
        }
        if let Some((path, pending)) = row {
            self.insert(id, &path, pending.as_deref());
        }
        Ok(())
    }
    pub(super) fn occupied(&self, path: &str, entity: Option<i64>) -> bool {
        let key = PathName::new(path);
        self.indexed.contains_key(&key)
            || self
                .pending
                .get(&key)
                .is_some_and(|owners| owners.iter().any(|id| Some(*id) != entity))
    }
}
fn remove_owner(owners: &mut Owners, path: &PathName, id: i64) {
    if let Some(ids) = owners.get_mut(path) {
        ids.remove(&id);
        if ids.is_empty() {
            owners.remove(path);
        }
    }
}
