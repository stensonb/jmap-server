use std::collections::HashMap;

use crate::batch::LogAction;
use crate::leb128::Leb128;
use crate::raft::RaftId;
use crate::serialize::{serialize_changelog_key, DeserializeBigEndian, COLLECTION_PREFIX_LEN};
use crate::{
    AccountId, CollectionId, ColumnFamily, Direction, JMAPStore, Store, StoreError, WriteOperation,
};

pub type ChangeLogId = u64;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ChangeLogEntry {
    Insert(ChangeLogId),
    Update(ChangeLogId),
    Delete(ChangeLogId),
}

pub struct ChangeLog {
    pub changes: Vec<ChangeLogEntry>,
    pub from_change_id: ChangeLogId,
    pub to_change_id: ChangeLogId,
}

impl Default for ChangeLog {
    fn default() -> Self {
        Self {
            changes: Vec::with_capacity(10),
            from_change_id: 0,
            to_change_id: 0,
        }
    }
}

#[derive(Debug)]
pub enum ChangeLogQuery {
    All,
    Since(ChangeLogId),
    SinceInclusive(ChangeLogId),
    RangeInclusive(ChangeLogId, ChangeLogId),
}

impl ChangeLog {
    pub fn deserialize(&mut self, bytes: &[u8]) -> Option<()> {
        let mut bytes_it = bytes.iter();
        let total_inserts = usize::from_leb128_it(&mut bytes_it)?;
        let total_updates = usize::from_leb128_it(&mut bytes_it)?;
        let total_deletes = usize::from_leb128_it(&mut bytes_it)?;

        if total_inserts > 0 {
            for _ in 0..total_inserts {
                self.changes
                    .push(ChangeLogEntry::Insert(ChangeLogId::from_leb128_it(
                        &mut bytes_it,
                    )?));
            }
        }

        if total_updates > 0 {
            'update_outer: for _ in 0..total_updates {
                let id = ChangeLogId::from_leb128_it(&mut bytes_it)?;

                if !self.changes.is_empty() {
                    let mut update_idx = None;
                    for (idx, change) in self.changes.iter().enumerate() {
                        match change {
                            ChangeLogEntry::Insert(insert_id) => {
                                if *insert_id == id {
                                    // Item updated after inserted, no need to count this change.
                                    continue 'update_outer;
                                }
                            }
                            ChangeLogEntry::Update(update_id) => {
                                if *update_id == id {
                                    update_idx = Some(idx);
                                    break;
                                }
                            }
                            _ => (),
                        }
                    }

                    // Move update to the front
                    if let Some(idx) = update_idx {
                        self.changes.remove(idx);
                    }
                }

                self.changes.push(ChangeLogEntry::Update(id));
            }
        }

        if total_deletes > 0 {
            'delete_outer: for _ in 0..total_deletes {
                let id = ChangeLogId::from_leb128_it(&mut bytes_it)?;

                if !self.changes.is_empty() {
                    let mut update_idx = None;
                    for (idx, change) in self.changes.iter().enumerate() {
                        match change {
                            ChangeLogEntry::Insert(insert_id) => {
                                if *insert_id == id {
                                    self.changes.remove(idx);
                                    continue 'delete_outer;
                                }
                            }
                            ChangeLogEntry::Update(update_id) => {
                                if *update_id == id {
                                    update_idx = Some(idx);
                                    break;
                                }
                            }
                            _ => (),
                        }
                    }
                    if let Some(idx) = update_idx {
                        self.changes.remove(idx);
                    }
                }

                self.changes.push(ChangeLogEntry::Delete(id));
            }
        }

        Some(())
    }
}

#[derive(Default)]
pub struct LogEntry {
    pub inserts: Vec<ChangeLogId>,
    pub updates: Vec<ChangeLogId>,
    pub deletes: Vec<ChangeLogId>,
}

impl From<LogEntry> for Vec<u8> {
    fn from(writer: LogEntry) -> Self {
        writer.serialize()
    }
}

//TODO delete old changelog entries
impl LogEntry {
    pub fn new() -> Self {
        LogEntry::default()
    }

    pub fn serialize(self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            (self.inserts.len() + self.updates.len() + self.deletes.len() + 3)
                * std::mem::size_of::<usize>(),
        );
        self.inserts.len().to_leb128_bytes(&mut buf);
        self.updates.len().to_leb128_bytes(&mut buf);
        self.deletes.len().to_leb128_bytes(&mut buf);
        for list in [self.inserts, self.updates, self.deletes] {
            for id in list {
                id.to_leb128_bytes(&mut buf);
            }
        }
        buf
    }
}

pub struct LogWriter {
    pub account_id: AccountId,
    pub raft_id: RaftId,
    pub changes: HashMap<(CollectionId, ChangeLogId), LogEntry>,
}

impl LogWriter {
    pub fn new(account_id: AccountId, raft_id: RaftId) -> Self {
        LogWriter {
            account_id,
            raft_id,
            changes: HashMap::new(),
        }
    }

    pub fn add_change(
        &mut self,
        collection_id: CollectionId,
        change_id: ChangeLogId,
        action: LogAction,
    ) {
        let log_entry = self
            .changes
            .entry((collection_id, change_id))
            .or_insert_with(LogEntry::new);

        match action {
            LogAction::Insert(id) => {
                log_entry.inserts.push(id);
            }
            LogAction::Update(id) => {
                log_entry.updates.push(id);
            }
            LogAction::Delete(id) => {
                log_entry.deletes.push(id);
            }
            LogAction::Move(old_id, id) => {
                log_entry.inserts.push(id);
                log_entry.deletes.push(old_id);
            }
        }
    }

    pub fn serialize(self, batch: &mut Vec<WriteOperation>) {
        let mut raft_bytes = Vec::with_capacity(
            std::mem::size_of::<AccountId>()
                + std::mem::size_of::<usize>()
                + (self.changes.len()
                    * (std::mem::size_of::<ChangeLogId>() + std::mem::size_of::<CollectionId>())),
        );

        self.account_id.to_leb128_bytes(&mut raft_bytes);
        self.changes.len().to_leb128_bytes(&mut raft_bytes);

        for ((collection_id, change_id), log_entry) in self.changes {
            collection_id.to_leb128_bytes(&mut raft_bytes);
            change_id.to_leb128_bytes(&mut raft_bytes);

            batch.push(WriteOperation::set(
                ColumnFamily::Logs,
                serialize_changelog_key(self.account_id, collection_id, change_id),
                log_entry.serialize(),
            ));
        }

        batch.push(WriteOperation::set(
            ColumnFamily::Logs,
            self.raft_id.serialize_key(),
            raft_bytes,
        ));
    }
}

impl<T> JMAPStore<T>
where
    T: for<'x> Store<'x> + 'static,
{
    pub fn get_last_change_id(
        &self,
        account: AccountId,
        collection: CollectionId,
    ) -> crate::Result<Option<ChangeLogId>> {
        let key = serialize_changelog_key(account, collection, ChangeLogId::MAX);
        let key_len = key.len();

        if let Some((key, _)) = self
            .db
            .iterator(ColumnFamily::Logs, &key, Direction::Backward)?
            .into_iter()
            .next()
        {
            if key.starts_with(&key[0..COLLECTION_PREFIX_LEN]) && key.len() == key_len {
                return Ok(Some(
                    key.as_ref()
                        .deserialize_be_u64(COLLECTION_PREFIX_LEN)
                        .ok_or_else(|| {
                            StoreError::InternalError(format!(
                                "Corrupted changelog key for [{}/{}]: [{:?}]",
                                account, collection, key
                            ))
                        })?,
                ));
            }
        }
        Ok(None)
    }

    pub fn get_changes(
        &self,
        account: AccountId,
        collection: CollectionId,
        query: ChangeLogQuery,
    ) -> crate::Result<Option<ChangeLog>> {
        let mut changelog = ChangeLog::default();
        /*let (is_inclusive, mut match_from_change_id, from_change_id, to_change_id) = match query {
            ChangeLogQuery::All => (true, false, 0, 0),
            ChangeLogQuery::Since(change_id) => (false, true, change_id, 0),
            ChangeLogQuery::SinceInclusive(change_id) => (true, true, change_id, 0),
            ChangeLogQuery::RangeInclusive(from_change_id, to_change_id) => {
                (true, true, from_change_id, to_change_id)
            }
        };*/
        let (is_inclusive, from_change_id, to_change_id) = match query {
            ChangeLogQuery::All => (true, 0, 0),
            ChangeLogQuery::Since(change_id) => (false, change_id, 0),
            ChangeLogQuery::SinceInclusive(change_id) => (true, change_id, 0),
            ChangeLogQuery::RangeInclusive(from_change_id, to_change_id) => {
                (true, from_change_id, to_change_id)
            }
        };
        let key = serialize_changelog_key(account, collection, from_change_id);
        let key_len = key.len();
        let prefix = &key[0..COLLECTION_PREFIX_LEN];
        let mut is_first = true;

        for (key, value) in self
            .db
            .iterator(ColumnFamily::Logs, &key, Direction::Forward)?
        {
            if !key.starts_with(prefix) {
                break;
            } else if key.len() != key_len {
                //TODO avoid collisions with Raft keys
                continue;
            }
            let change_id = key
                .as_ref()
                .deserialize_be_u64(COLLECTION_PREFIX_LEN)
                .ok_or_else(|| {
                    StoreError::InternalError(format!(
                        "Failed to deserialize changelog key for [{}/{}]: [{:?}]",
                        account, collection, key
                    ))
                })?;

            /*if match_from_change_id {
                if change_id != from_change_id {
                    return Ok(None);
                } else {
                    match_from_change_id = false;
                }
            }*/

            if change_id > from_change_id || (is_inclusive && change_id == from_change_id) {
                if to_change_id > 0 && change_id > to_change_id {
                    break;
                }
                if is_first {
                    changelog.from_change_id = change_id;
                    is_first = false;
                }
                changelog.to_change_id = change_id;
                changelog.deserialize(&value).ok_or_else(|| {
                    StoreError::InternalError(format!(
                        "Failed to deserialize changelog for [{}/{}]: [{:?}]",
                        account, collection, query
                    ))
                })?;
            }
        }

        if is_first {
            changelog.from_change_id = from_change_id;
            changelog.to_change_id = if to_change_id > 0 {
                to_change_id
            } else {
                from_change_id
            };
        }

        Ok(Some(changelog))
    }
}