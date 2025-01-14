/*
 * Copyright (c) 2020-2022, Stalwart Labs Ltd.
 *
 * This file is part of the Stalwart JMAP Server.
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of
 * the License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 * GNU Affero General Public License for more details.
 * in the LICENSE file at the top-level directory of this distribution.
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 *
 * You can be released from the requirements of the AGPLv3 license by
 * purchasing a commercial license. Please contact licensing@stalw.art
 * for more details.
*/

use std::collections::hash_map::Entry;

use serde::{Deserialize, Serialize};
use store::{
    ahash::AHashMap,
    core::{
        acl::{Permission, ACL},
        bitmap::Bitmap,
        vec_map::VecMap,
    },
    AccountId,
};

use crate::jmap_store::Object;

use super::TinyORM;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ACLUpdate {
    Replace {
        acls: VecMap<String, Vec<ACL>>,
    },
    Update {
        account_id: String,
        acls: Vec<ACL>,
    },
    Set {
        account_id: String,
        acl: ACL,
        is_set: bool,
    },
}

impl<T> TinyORM<T>
where
    T: Object + 'static,
{
    pub fn acl_revoke(&mut self, account_id: AccountId) {
        if let Some(pos) = self.acls.iter().position(|p| p.id == account_id) {
            self.acls.swap_remove(pos);
        }
    }

    pub fn acl_update(&mut self, account_id: AccountId, acl: impl Into<Bitmap<ACL>>) {
        let acl = acl.into();
        if !acl.is_empty() {
            if let Some(permission) = self.acls.iter_mut().find(|p| p.id == account_id) {
                if permission.acl != acl {
                    permission.acl = acl;
                }
            } else {
                self.acls.push(Permission {
                    id: account_id,
                    acl,
                });
            }
        } else {
            self.acl_revoke(account_id);
        }
    }

    pub fn acl_set(&mut self, account_id: AccountId, acl: ACL, is_set: bool) {
        if acl != ACL::None_ {
            if let Some(permission) = self.acls.iter_mut().find(|p| p.id == account_id) {
                if is_set {
                    permission.acl.insert(acl);
                } else {
                    permission.acl.remove(acl);
                    if permission.acl.is_empty() {
                        self.acl_revoke(account_id);
                    }
                }
            } else if is_set {
                self.acls.push(Permission {
                    id: account_id,
                    acl: acl.into(),
                });
            }
        }
    }

    pub fn acl_clear(&mut self) {
        self.acls.clear();
    }

    pub fn acl_finish(&mut self) {
        self.acls.sort_unstable();
    }

    pub fn acl_check(&self, account_id: AccountId, acl: ACL) -> bool {
        self.acls
            .iter()
            .find(|p| p.id == account_id)
            .map_or(false, |p| p.acl.contains(acl))
    }

    pub fn get_acls(&self) -> impl Iterator<Item = (AccountId, Vec<ACL>)> + '_ {
        self.acls
            .iter()
            .map(|acl| (acl.id, acl.acl.clone().into_iter().collect()))
    }

    pub fn get_changed_acls(&self, changes: Option<&Self>) -> Option<Vec<Permission>> {
        if let Some(changes) = changes {
            if changes.acls != self.acls {
                let mut acls: AHashMap<AccountId, Bitmap<ACL>> = AHashMap::default();
                for (a, b) in [(&self.acls, &changes.acls), (&changes.acls, &self.acls)] {
                    for p in a {
                        if !b.contains(p) {
                            match acls.entry(p.id) {
                                Entry::Occupied(mut entry) => {
                                    entry.get_mut().union(&p.acl);
                                }
                                Entry::Vacant(entry) => {
                                    entry.insert(p.acl.clone());
                                }
                            }
                        }
                    }
                }
                acls.into_iter()
                    .map(|(id, acl)| Permission { id, acl })
                    .collect::<Vec<_>>()
                    .into()
            } else {
                None
            }
        } else if !self.acls.is_empty() {
            self.acls.clone().into()
        } else {
            None
        }
    }
}
