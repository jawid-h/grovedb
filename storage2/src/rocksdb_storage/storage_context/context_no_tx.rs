use rocksdb::{ColumnFamily, Error, WriteBatchWithTransaction};

use super::{make_prefixed_key, Db, PrefixedRocksDbBatch, PrefixedRocksDbRawIterator};
use crate::{
    rocksdb_storage::storage::{AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    StorageContext,
};

/// Storage context with a prefix applied to be used in a subtree to be used
/// outside of transaction.
pub struct PrefixedRocksDbStorageContext<'a> {
    storage: &'a Db,
    prefix: Vec<u8>,
}

impl<'a> PrefixedRocksDbStorageContext<'a> {
    /// Create a new prefixed storage context instance
    pub fn new(storage: &'a Db, prefix: Vec<u8>) -> Self {
        PrefixedRocksDbStorageContext { storage, prefix }
    }
}

impl<'a> PrefixedRocksDbStorageContext<'a> {
    /// Get auxiliary data column family
    fn cf_aux(&self) -> &ColumnFamily {
        self.storage
            .cf_handle(AUX_CF_NAME)
            .expect("aux column family must exist")
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> &ColumnFamily {
        self.storage
            .cf_handle(ROOTS_CF_NAME)
            .expect("roots column family must exist")
    }

    /// Get metadata column family
    fn cf_meta(&self) -> &ColumnFamily {
        self.storage
            .cf_handle(META_CF_NAME)
            .expect("meta column family must exist")
    }
}

impl<'a> StorageContext<'a> for PrefixedRocksDbStorageContext<'a> {
    type Batch = PrefixedRocksDbBatch<'a, WriteBatchWithTransaction<true>>;
    type Error = Error;
    type RawIterator = PrefixedRocksDbRawIterator;

    fn put<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.storage
            .put(make_prefixed_key(self.prefix.clone(), key), value)
    }

    fn put_aux<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.storage.put_cf(
            self.cf_aux(),
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn put_root<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.storage.put_cf(
            self.cf_roots(),
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn put_meta<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.storage.put_cf(
            self.cf_meta(),
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.storage
            .delete(make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.storage
            .delete_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_root<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.storage
            .delete_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.storage
            .delete_cf(self.cf_meta(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        self.storage
            .get(make_prefixed_key(self.prefix.clone(), key))
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        self.storage
            .get_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        self.storage
            .get_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        self.storage
            .get_cf(self.cf_meta(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn new_batch(&'a self) -> Self::Batch {
        PrefixedRocksDbBatch {
            prefix: self.prefix.clone(),
            batch: WriteBatchWithTransaction::<true>::default(),
            cf_aux: self.cf_aux(),
            cf_roots: self.cf_roots(),
        }
    }

    fn commit_batch(&self, batch: Self::Batch) -> Result<(), Self::Error> {
        self.storage.write(batch.batch)
    }

    fn raw_iter(&self) -> Self::RawIterator {
        todo!()
    }
}
