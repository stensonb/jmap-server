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

use super::{Cluster, PeerId};
use crate::JMAPServer;
use std::time::{Duration, Instant};
use store::log::raft::LogIndex;
use store::tracing::{debug, error};
use store::Store;
use tokio::time;

impl<T> Cluster<T>
where
    T: for<'x> Store<'x> + 'static,
{
    pub async fn advance_commit_index(
        &mut self,
        peer_id: PeerId,
        commit_index: LogIndex,
    ) -> store::Result<bool> {
        let mut indexes = Vec::with_capacity(self.peers.len() + 1);
        for peer in self.peers.iter_mut() {
            if peer.is_in_shard(self.shard_id) {
                if peer.peer_id == peer_id
                    && (commit_index > peer.commit_index || peer.commit_index == LogIndex::MAX)
                {
                    peer.commit_index = commit_index;
                }
                indexes.push(peer.commit_index.wrapping_add(1));
            }
        }
        indexes.push(self.uncommitted_index.wrapping_add(1));
        indexes.sort_unstable();

        // Use div_floor when stabilized.
        let commit_index = indexes[((indexes.len() as f64) / 2.0).floor() as usize];
        if commit_index > self.last_log.index.wrapping_add(1) {
            self.last_log.index = commit_index.wrapping_sub(1);
            self.last_log.term = self.term;

            let last_log_index = self.last_log.index;
            let core = self.core.clone();

            // Commit pending updates
            tokio::spawn(async move {
                if let Err(err) = core.commit_leader(last_log_index, false).await {
                    error!("Failed to commit leader: {:?}", err);
                }
            });

            // Notify peers
            self.send_append_entries();

            // Notify clients
            if let Err(err) = self.commit_index_tx.send(last_log_index) {
                error!("Failed to send commit index: {:?}", err);
            }

            debug!(
                "Advancing commit index to {} [cluster: {:?}].",
                self.last_log.index, indexes
            );
        }
        Ok(true)
    }
}

impl<T> JMAPServer<T>
where
    T: for<'x> Store<'x> + 'static,
{
    pub async fn commit_index(&self, index: LogIndex) -> bool {
        if let Some(cluster) = &self.cluster {
            if self.is_leader() {
                if cluster
                    .tx
                    .send(crate::cluster::Event::AdvanceUncommittedIndex {
                        uncommitted_index: index,
                    })
                    .await
                    .is_ok()
                {
                    let commit_timeout = self.store.config.raft_commit_timeout;
                    let mut commit_index_rx = cluster.commit_index_rx.clone();
                    let wait_start = Instant::now();
                    let mut wait_timeout = Duration::from_millis(commit_timeout);

                    loop {
                        match time::timeout(wait_timeout, commit_index_rx.changed()).await {
                            Ok(Ok(())) => {
                                let commit_index = *commit_index_rx.borrow();
                                if commit_index >= index {
                                    debug!(
                                        "Successfully committed index {} in {}ms (latest index: {}).",
                                        index, wait_start.elapsed().as_millis(), commit_index
                                    );
                                    return true;
                                }

                                let wait_elapsed = wait_start.elapsed().as_millis() as u64;
                                if wait_elapsed >= commit_timeout {
                                    break;
                                }
                                wait_timeout = Duration::from_millis(commit_timeout - wait_elapsed);
                            }
                            Ok(Err(err)) => {
                                error!(
                                    "Failed to commit index {}, channel failure: {}",
                                    index, err
                                );
                                break;
                            }
                            Err(_) => {
                                error!(
                                    "Failed to commit index {}, timeout after {} ms.",
                                    index, commit_timeout
                                );
                                break;
                            }
                        }
                    }
                } else {
                    error!(
                        "Failed to commit index {}, unable to send store changed event.",
                        index
                    );
                }
            } else {
                error!(
                    "Failed to commit index {}, this node is no longer the leader.",
                    index
                );
            }
        }
        false
    }
}
