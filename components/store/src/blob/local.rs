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

use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    ops::Range,
    path::PathBuf,
};

use crate::{config::env_settings::EnvSettings, write::mutex_map::MutexMap};

use super::{BlobId, BlobStore};

pub struct LocalBlobStore {
    pub lock: MutexMap<()>,
    pub base_path: PathBuf,
    pub hash_levels: usize,
}

impl BlobStore for LocalBlobStore {
    fn new(settings: &EnvSettings) -> crate::Result<Self> {
        let mut base_path = PathBuf::from(
            settings
                .get("db-path")
                .unwrap_or_else(|| "/usr/local/stalwart-jmap/data".to_string()),
        );
        base_path.push("blobs");
        Ok(LocalBlobStore {
            lock: MutexMap::with_capacity(1024),
            base_path,
            hash_levels: std::cmp::min(settings.parse("blob-nested-levels").unwrap_or(2), 5),
        })
    }

    fn put(&self, blob_id: &BlobId, blob: &[u8]) -> crate::Result<bool> {
        let blob_path = self.get_path(blob_id)?;

        if blob_path.exists() {
            let metadata = fs::metadata(&blob_path)?;
            if metadata.len() as usize == blob.len() {
                return Ok(false);
            }
        }

        fs::create_dir_all(blob_path.parent().unwrap())?;
        let mut blob_file = File::create(&blob_path)?;
        blob_file.write_all(blob)?;
        blob_file.flush()?;

        Ok(true)
    }

    fn get_range(&self, blob_id: &BlobId, range: Range<u32>) -> crate::Result<Option<Vec<u8>>> {
        let blob_path = self.get_path(blob_id)?;
        if !blob_path.exists() {
            return Ok(None);
        }

        let blob_size = fs::metadata(&blob_path)?.len();
        let mut blob = File::open(&blob_path)?;
        Ok(Some(if range.start != 0 || range.end != u32::MAX {
            let from_offset = if range.start < blob_size as u32 {
                range.start
            } else {
                0
            };
            let mut buf =
                vec![0; (std::cmp::min(range.end, blob_size as u32) - from_offset) as usize];

            if from_offset > 0 {
                blob.seek(SeekFrom::Start(from_offset as u64))?;
            }
            blob.read_exact(&mut buf)?;
            buf
        } else {
            let mut buf = Vec::with_capacity(blob_size as usize);
            blob.read_to_end(&mut buf)?;
            buf
        }))
    }

    fn delete(&self, blob_id: &BlobId) -> crate::Result<bool> {
        let blob_path = self.get_path(blob_id)?;
        if blob_path.exists() {
            fs::remove_file(&blob_path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl LocalBlobStore {
    fn get_path(&self, blob_id: &BlobId) -> crate::Result<PathBuf> {
        let mut path = self.base_path.clone();
        let hash = blob_id.hash();
        for byte in hash.iter().take(self.hash_levels) {
            path.push(format!("{:x}", byte));
        }
        path.push(blob_id.to_string());

        Ok(path)
    }
}
